// A Flutter app with the core from agentim_client's build hook,
// against a real agentim: two people connect, one sends, the other gets it
// through its view; the widget shows the conversation. With AGENTIM_SMOKE
// set it prints the result and exits (CI, see test.sh).

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:agentim_client/agentim_client.dart';
import 'package:flutter/widgets.dart';

void main() => runApp(const App());

class App extends StatefulWidget {
  const App({super.key});
  @override
  State<App> createState() => _AppState();
}

class _AppState extends State<App> {
  final lines = <String>[];

  void log(String s) {
    // print: on Android it goes to logcat (stdout does not).
    print(s); // ignore: avoid_print
    setState(() => lines.add(s));
  }

  @override
  void initState() {
    super.initState();
    unawaited(run());
  }

  Future<Map<String, dynamic>> person(String url, String name) async {
    final http = HttpClient();
    final req = await http.postUrl(Uri.parse('$url/api/dev/users'));
    req.headers.contentType = ContentType.json;
    req.write(jsonEncode({'name': name}));
    final r = jsonDecode(await (await req.close()).transform(utf8.decoder).join()) as Map<String, dynamic>;
    http.close();
    return r;
  }

  Future<bool> until(bool Function() f) async {
    for (var i = 0; i < 400; i++) {
      if (f()) return true;
      await Future.delayed(const Duration(milliseconds: 50));
    }
    return f();
  }

  Future<void> run() async {
    // An app on a phone has no environment: --dart-define=AGENTIM_URL=...
    final url = Platform.environment['AGENTIM_URL'] ?? const String.fromEnvironment('AGENTIM_URL', defaultValue: 'http://127.0.0.1:8080');
    var ok = true;
    void check(bool c, String what) {
      log('${c ? 'ok  ' : 'FAIL'} $what');
      if (!c) ok = false;
    }

    try {
      check(AgentIMClient.version().isNotEmpty, 'the bundled core loads (${AgentIMClient.version()})');
      final tag = DateTime.now().millisecondsSinceEpoch;
      final alice = await person(url, 'flutter-a$tag');
      final bob = await person(url, 'flutter-b$tag');
      AgentIMClient client(Map<String, dynamic> p) =>
          AgentIMClient.create(ClientOptions(url: url, deviceId: '${p['user']['name']}-d', tokenProvider: () async => p['token'] as String));
      final a = client(alice);
      final b = client(bob);
      await Future.wait([a.connect(), b.connect()]);
      check(await until(() => a.connection.value.state == 'connected' && b.connection.value.state == 'connected'), 'both connect');
      final conv =
          ((await a.call('create_conv', {
                    'kind': 'group',
                    'title': 'flutter',
                    'members': [bob['user']['id']],
                  }))
                  as Map)['conv_id']
              as String;
      final bv = b.messages(conv);
      await a.send(conv, 'hello from a Flutter app 👋');
      check(await until(() => bv.list.value.any((m) => m['content'] == 'hello from a Flutter app 👋' && m['status'] == 'sent')), 'the other side gets it');
      a.dispose();
      b.dispose();
    } catch (e) {
      check(false, '$e');
    }
    log(ok ? 'AGENTIM_SMOKE_OK' : 'AGENTIM_SMOKE_FAILED');
    if (Platform.environment.containsKey('AGENTIM_SMOKE')) exit(ok ? 0 : 1);
  }

  @override
  Widget build(BuildContext context) => Directionality(
    textDirection: TextDirection.ltr,
    child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [for (final l in lines) Text(l)]),
  );
}
