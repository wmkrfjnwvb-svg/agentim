// The wrapper conformance scenario (docs/12 §2.13, W1–W9), against a real
// `agentim`: the same steps every platform wrapper runs. See test.sh.

import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:agentim_client/agentim_client.dart';

var failures = 0;

void check(bool ok, String what) {
  print('${ok ? 'ok  ' : 'FAIL'} $what');
  if (!ok) failures++;
}

Future<bool> until(Future<bool> Function() f, [int ms = 20000]) async {
  final end = DateTime.now().add(Duration(milliseconds: ms));
  while (DateTime.now().isBefore(end)) {
    if (await f()) return true;
    await Future.delayed(const Duration(milliseconds: 25));
  }
  return f();
}

Future<bool> untilSync(bool Function() f) => until(() async => f());

List<String> sent(Messages v) => v.list.value.where((m) => m['status'] == 'sent').map((m) => m['content'] as String).toList();

Future<void> main() async {
  // B12: a reset replaces a view in one update, never empty in between.
  {
    Map<String, dynamic> up(String key, int order) => {
      'op': 'upsert',
      'message': {'key': key, 'order': order},
    };
    final v = Messages.detached('c1');
    v.debugApplyEvent({
      'changes': [up('a', 1), up('b', 2)],
    });
    final lists = <List<String>>[];
    final sub = v.list.watch().skip(1).listen((l) => lists.add([for (final m in l) m['key'] as String]));
    await Future<void>.delayed(Duration.zero);
    v.debugApplyEvent({
      'reset': true,
      'changes': [up('b', 2), up('c', 3)],
    });
    await Future<void>.delayed(Duration.zero);
    await sub.cancel();
    check(lists.length == 1 && lists[0].join(',') == 'b,c', 'B12 a reset is one update ($lists)');
  }

  final env = Platform.environment;
  final bin = env['AGENTIM_BIN']!;
  final dir = Directory.systemTemp.createTempSync('agentim-dart-');
  final port = 20150 + Random().nextInt(100);
  final url = 'http://127.0.0.1:$port';
  final server = await Process.start(bin, ['--demo', '--db', '${dir.path}/a.db', '--addr', '127.0.0.1:$port']);
  server.stdout.drain<void>();
  server.stderr.drain<void>();
  final http = HttpClient();
  var n = 0;

  Future<({String id, String name, String token})> person(String name) async {
    final nm = '$name${++n}';
    final req = await http.postUrl(Uri.parse('$url/api/dev/users'));
    req.headers.contentType = ContentType.json;
    req.write(jsonEncode({'name': nm}));
    final r = jsonDecode(await (await req.close()).transform(utf8.decoder).join()) as Map;
    return (id: r['user']['id'] as String, name: nm, token: r['token'] as String);
  }

  AgentIMClient client(({String id, String name, String token}) p, {String? store, String? device, Future<String> Function()? token}) =>
      AgentIMClient.create(ClientOptions(url: url, deviceId: device ?? '${p.name}-d', storeDir: store, tokenProvider: token ?? () async => p.token));

  Future<String> code(Future<Object?> Function() f) async {
    try {
      await f();
      return '';
    } on AgentIMError catch (e) {
      return e.code;
    }
  }

  try {
    await until(() async {
      try {
        final r = await (await http.getUrl(Uri.parse('$url/readyz'))).close();
        await r.drain<void>();
        return r.statusCode == 200;
      } catch (_) {
        return false;
      }
    }, 30000);
    final version = AgentIMClient.version();
    check(version.isNotEmpty, 'version $version');
    final alice = await person('alice');
    final bob = await person('bob');
    final a = client(alice);
    final b = client(bob);
    await Future.wait([a.connect(), b.connect()]);
    check(await untilSync(() => a.connection.value.state == 'connected' && b.connection.value.state == 'connected'), 'W1 both connect');

    final conv =
        ((await a.call('create_conv', {
                  'kind': 'group',
                  'title': 'dart',
                  'members': [bob.id],
                }))
                as Map)['conv_id']
            as String;
    final av = a.messages(conv);
    final r = await a.send(conv, 'hello from Dart 👋');
    check((r['key'] as String).startsWith('local:') && av.list.value.any((m) => m['content'] == 'hello from Dart 👋'), 'W2 a send is shown when it returns');
    check(await untilSync(() => sent(av).contains('hello from Dart 👋')), 'W2 and then sent (emoji intact)');
    final bv = b.messages(conv);
    check(await untilSync(() => sent(bv).contains('hello from Dart 👋')), 'W2 the other side sees it');
    check(await untilSync(() => b.conversations.value.any((c) => c['conv_id'] == conv)), 'W2 and has the conversation in its list');

    final mine = av.list.value.firstWhere((m) => m['content'] == 'hello from Dart 👋');
    await a.edit(conv, mine['msg_id'] as String, 'hello, edited');
    check(await untilSync(() => sent(bv).firstOrNull == 'hello, edited'), 'W3 an edit reaches the other side');
    await a.recall(conv, mine['msg_id'] as String);
    check(await untilSync(() => bv.list.value.firstOrNull?['state'] == 'recalled'), 'W3 a recall reaches the other side');

    final ops = <String>[];
    final off = a.on('operation', (e) => ops.add(e['result'] as String? ?? ''));
    await a.send('c_not_mine', 'nope', clientMsgId: 'bad1');
    final nv = a.messages('c_not_mine');
    check(await untilSync(() => nv.list.value.any((m) => m['status'] == 'failed') && ops.contains('failed')), 'W4 a refused send fails and is reported');
    await a.discard('c_not_mine', 'bad1');
    check(await untilSync(() => nv.list.value.isEmpty), 'W4 discard removes it');
    off();

    final c1 = await code(() => a.call('get_request', {'request_id': 'nope'}));
    check(c1 == 'not_found', "W5 an online call answers with the server's code ($c1)");
    final c2 = await code(() => a.command({'cmd': 'no_such_command'}));
    check(c2 == 'bad_request', 'W5 an unknown command is bad_request ($c2)');

    final store = '${dir.path}/alice-cache';
    var q = client(alice, store: store, device: 'alice-q');
    for (final t in ['one', 'two', 'three']) {
      await q.send(conv, t);
    }
    q.dispose();
    q = client(alice, store: store, device: 'alice-q');
    final v = q.messages(conv);
    check(await untilSync(() => v.list.value.where((m) => m['status'] == 'sending').length == 3), 'W6 after a restart the queue shows before connecting');
    await q.connect();
    check(await untilSync(() => sent(bv).where((t) => ['one', 'two', 'three'].contains(t)).join('|') == 'one|two|three'), 'W6 and is sent once, in order');
    check(await until(() async => ((await q.command({'cmd': 'pending'})) as Map)['ops'].isEmpty as bool), 'W6 and the queue is empty');
    q.setNetwork(false);
    check(await untilSync(() => q.connection.value.state != 'connected'), 'W7 offline is not connected (${q.connection.value.state})');
    await q.send(conv, 'while offline');
    q.setNetwork(true);
    check(await untilSync(() => sent(bv).where((t) => t == 'while offline').length == 1), 'W7 back online it is sent once');
    q.dispose();

    final x = client(alice, device: 'alice-r', token: () async => throw const TokenRefused());
    await x.connect();
    check(await untilSync(() => x.connection.value.state == 'failed'), 'W8 a refused token stops the client');
    check(x.connection.value.reason == 'unauthenticated', 'W8 as unauthenticated (${x.connection.value.reason})');
    x.dispose();

    final d = client(alice, device: 'alice-d');
    d.dispose();
    d.dispose();
    final c3 = await code(() => d.connect());
    check(c3 == 'closed', 'W9 after dispose, commands fail with closed (dispose twice is fine)');
    a.dispose();
    b.dispose();
  } catch (e, st) {
    print('FAIL $e\n$st');
    failures++;
  } finally {
    server.kill();
    http.close(force: true);
    dir.deleteSync(recursive: true);
  }
  exit(failures == 0 ? 0 : 1);
}
