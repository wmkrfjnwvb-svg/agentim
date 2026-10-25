// AgentIM client for Flutter and Dart (docs/12 §2). The rules live in the
// Rust core; this turns its events into values with streams and its
// replies into futures. Everything arrives on this isolate's event loop in
// the core's order: when a future completes, the state it changed is there.
//
//   final client = AgentIMClient.create(ClientOptions(
//     url: 'https://chat.example.com',
//     deviceId: savedDeviceId,
//     storeDir: '${(await getApplicationSupportDirectory()).path}/agentim',
//     tokenProvider: () => backend.token(),
//   ));
//   StreamBuilder(stream: client.conversations.watch(), builder: …);
//   await client.connect();
//   await client.send(convId, 'hello');   // in messages(convId).list when it returns
//
// The native core comes from this package's build hook (hook/build.dart).
@DefaultAsset('package:agentim_client/agentim_client.dart')
library;

import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io' show Platform;

import 'package:ffi/ffi.dart';

class ClientOptions {
  /// `https://chat.example.com`.
  final String url;

  /// This device's id, the same every run (1..64 of A-Z a-z 0-9 _ -).
  final String deviceId;

  /// A client token from your backend; throw [TokenRefused] to stop.
  final Future<String> Function() tokenProvider;

  /// Where the cache lives; null keeps it in memory.
  final String? storeDir;
  final String? deviceName;
  final String? platform;
  final String clientName;
  final String clientVersion;
  final Duration commandTimeout;

  const ClientOptions({
    required this.url,
    required this.deviceId,
    required this.tokenProvider,
    this.storeDir,
    this.deviceName,
    this.platform,
    this.clientName = 'agentim-dart',
    this.clientVersion = '0.1.0',
    this.commandTimeout = const Duration(seconds: 60),
  });
}

/// Thrown by a token provider when the backend refuses the user.
class TokenRefused implements Exception {
  final String message;
  const TokenRefused([this.message = 'refused']);
}

/// The server's code (`forbidden`, `not_found`, `conflict`, `bad_request`, `blocked`,
/// `rate_limited`…) or the client's (`disconnected`, `timeout`, `closed`…); unknown codes
/// pass through as they are.
class AgentIMError implements Exception {
  final String code;
  final String message;
  final bool retryable;
  final int? retryAfterMs;
  const AgentIMError(this.code, this.message, {this.retryable = false, this.retryAfterMs});

  factory AgentIMError.from(Map<String, dynamic>? e) => AgentIMError(
    e?['code'] as String? ?? 'internal',
    e?['message'] as String? ?? e?['code'] as String? ?? 'error',
    retryable: e?['retryable'] as bool? ?? false,
    retryAfterMs: (e?['retry_after_ms'] as num?)?.toInt(),
  );

  @override
  String toString() => 'AgentIMError($code: $message)';
}

/// A value the UI follows: [value] stays the same object until it changes.
class Observed<T> {
  T _value;
  final _changes = StreamController<T>.broadcast(sync: true);
  Observed(this._value);

  T get value => _value;

  /// The current value, then every change.
  Stream<T> watch() async* {
    yield _value;
    yield* _changes.stream;
  }

  void _set(T v) {
    _value = v;
    _changes.add(v);
  }
}

class Connection {
  /// closed, connecting, connected, disconnected, suspended or failed.
  final String state;
  final String? reason;
  final int? retryAt;
  const Connection(this.state, [this.reason, this.retryAt]);
}

/// One conversation's messages as shown, oldest first; each has `key`, `content`,
/// `status` (sent, sending, failed), `state`, `sender_type`…
class Messages {
  final AgentIMClient? _client;
  final String convId;
  final list = Observed<List<Map<String, dynamic>>>(const []);
  bool hasOlder = false;
  String? olderEnd;
  final _byKey = <String, Map<String, dynamic>>{};

  Messages._(AgentIMClient this._client, this.convId);

  /// A view tied to no client, for tests (with [debugApplyEvent]).
  Messages.detached(this.convId) : _client = null;

  void _reset(List? messages, bool? older, String? end) {
    _byKey.clear();
    for (final m in messages ?? const []) {
      _byKey[m['key'] as String] = (m as Map).cast<String, dynamic>();
    }
    if (older != null) hasOlder = older;
    if (end != null) olderEnd = end;
    _publish();
  }

  /// A `messages` event: with `reset` the view is replaced by the changes
  /// in one update, so no one sees it empty in between.
  void _applyEvent(Map e) {
    if (e['reset'] == true) _byKey.clear();
    _apply(e['changes'] as List?);
  }

  /// Applies a `messages` event as the client does, for tests.
  void debugApplyEvent(Map<String, dynamic> e) => _applyEvent(e);

  void _apply(List? changes) {
    for (final c in changes ?? const []) {
      if (c['op'] == 'upsert') {
        final m = (c['message'] as Map).cast<String, dynamic>();
        _byKey[m['key'] as String] = m;
      } else {
        _byKey.remove(c['key']);
      }
    }
    _publish();
  }

  void _publish() {
    final l = _byKey.values.toList()..sort((a, b) => ((a['order'] as num?) ?? 0).compareTo((b['order'] as num?) ?? 0));
    list._set(List.unmodifiable(l));
  }

  /// The page before the oldest shown: the cache first, then the server. False at the start.
  Future<bool> loadOlder() async {
    final r = await _client!.command({'cmd': 'load_older', 'conv_id': convId}) as Map;
    hasOlder = r['has_older'] as bool? ?? false;
    olderEnd = r['older_end'] as String?;
    return hasOlder;
  }

  /// A page around a message (it stays loaded).
  Future<void> jumpTo(int seq) => _client!.command({'cmd': 'jump_to', 'conv_id': convId, 'seq': seq});
}

typedef _EventNative = Void Function(Pointer<Utf8>);

// The C ABI (sdk/client-rust/native/include/agentim.h).
@Native<Pointer<Void> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<NativeFunction<_EventNative>>)>()
external Pointer<Void> agentim_dart_new(Pointer<Utf8> config, Pointer<Utf8> storeDir, Pointer<NativeFunction<_EventNative>> callback);
@Native<Void Function(Pointer<Utf8>)>()
external void agentim_dart_string_free(Pointer<Utf8> json);
@Native<Int32 Function(Pointer<Void>, Uint64, Pointer<Utf8>)>()
external int agentim_client_command(Pointer<Void> c, int id, Pointer<Utf8> json);
@Native<Void Function(Pointer<Void>, Pointer<Utf8>, Int32)>()
external void agentim_client_token(Pointer<Void> c, Pointer<Utf8> token, int refused);
@Native<Void Function(Pointer<Void>, Int32, Uint64)>()
external void agentim_client_network(Pointer<Void> c, int online, int generation);
@Native<Void Function(Pointer<Void>, Int32)>()
external void agentim_client_foreground(Pointer<Void> c, int foreground);
@Native<Void Function(Pointer<Void>)>()
external void agentim_client_free(Pointer<Void> c);
@Native<Pointer<Utf8> Function()>()
external Pointer<Utf8> agentim_version();

class AgentIMClient {
  final ClientOptions _o;
  late final NativeCallable<_EventNative> _callable;
  Pointer<Void> _ptr = nullptr;
  int _next = 0;
  int _generation = 0;
  final _pending = <int, Completer<Object?>>{};
  // Applied in event order, before later events (a view's first snapshot).
  final _inOrder = <int, void Function(Object?)>{};
  final _views = <String, Messages>{};
  final _convs = <String, Map<String, dynamic>>{};
  final _listeners = <String, List<void Function(Map<String, dynamic>)>>{};

  final connection = Observed<Connection>(const Connection('closed'));
  final conversations = Observed<List<Map<String, dynamic>>>(const []);

  AgentIMClient._(this._o);

  static String version() => agentim_version().toDartString();

  static AgentIMClient create(ClientOptions o) {
    final c = AgentIMClient._(o);
    c._callable = NativeCallable<_EventNative>.listener(c._onEvent);
    final cfg = jsonEncode({
      'url': o.url,
      'device_id': o.deviceId,
      'device_name': o.deviceName,
      'platform': o.platform ?? Platform.operatingSystem,
      'client_name': o.clientName,
      'client_version': o.clientVersion,
    }).toNativeUtf8();
    final dir = o.storeDir?.toNativeUtf8() ?? nullptr;
    try {
      c._ptr = agentim_dart_new(cfg, dir.cast(), c._callable.nativeFunction);
    } finally {
      malloc.free(cfg);
      if (dir != nullptr) malloc.free(dir);
    }
    if (c._ptr == nullptr) {
      c._callable.close();
      throw const AgentIMError('bad_request', 'the client could not start (check url and device_id)');
    }
    return c;
  }

  void _onEvent(Pointer<Utf8> json) {
    final String text;
    try {
      text = json.toDartString();
    } finally {
      agentim_dart_string_free(json);
    }
    if (_ptr == nullptr) return;
    final e = (jsonDecode(text) as Map).cast<String, dynamic>();
    switch (e['type']) {
      case 'need_token':
        _provideToken();
        return;
      case 'reply':
        final id = e['id'] as int;
        final ok = e['ok'] == true;
        final apply = _inOrder.remove(id);
        if (ok) apply?.call(e['result']);
        final p = _pending.remove(id);
        if (ok) {
          p?.complete(e['result']);
        } else {
          p?.completeError(AgentIMError.from((e['error'] as Map?)?.cast<String, dynamic>()));
        }
        return;
      case 'connection':
        connection._set(Connection(e['state'] as String? ?? 'closed', e['reason'] as String?, (e['retry_at'] as num?)?.toInt()));
      case 'conversations':
        if (e['reset'] == true) _convs.clear();
        for (final ch in (e['changes'] as List?) ?? const []) {
          final conv = (ch['conversation'] as Map?)?.cast<String, dynamic>();
          if (conv != null) _convs[conv['conv_id'] as String] = conv;
        }
        int last(Map<String, dynamic> c) => ((c['last_message'] as Map?)?['created_at'] as num?)?.toInt() ?? 0;
        final list = _convs.values.where((c) => c['left'] != true && c['dissolved'] != true).toList()..sort((a, b) => last(b).compareTo(last(a)));
        conversations._set(List.unmodifiable(list));
      case 'messages':
        final v = _views[e['conv_id']];
        v?._applyEvent(e);
    }
    for (final f in List.of(_listeners[e['type']] ?? const <void Function(Map<String, dynamic>)>[])) {
      try {
        f(e);
      } catch (_) {
        // The listener's own error.
      }
    }
  }

  Future<void> _provideToken() async {
    String? token;
    var refused = 0;
    try {
      token = await _o.tokenProvider();
    } on TokenRefused {
      refused = 1;
    } catch (_) {
      // Failed: the core tries again later.
    }
    if (_ptr == nullptr) return;
    final t = token?.toNativeUtf8() ?? nullptr;
    agentim_client_token(_ptr, t.cast(), token == null ? refused : 0);
    if (t != nullptr) malloc.free(t);
  }

  /// Any core command (`{'cmd': …}`); completes with its result.
  Future<Object?> command(Map<String, Object?> cmd) => _command(cmd, null);

  Future<Object?> _command(Map<String, Object?> cmd, void Function(Object?)? applyInOrder) {
    if (_ptr == nullptr) return Future.error(const AgentIMError('closed', 'the client is closed'));
    final id = ++_next;
    final p = Completer<Object?>();
    _pending[id] = p;
    if (applyInOrder != null) _inOrder[id] = applyInOrder;
    final s = jsonEncode(cmd).toNativeUtf8();
    final r = agentim_client_command(_ptr, id, s);
    malloc.free(s);
    if (r != 0) {
      _pending.remove(id);
      _inOrder.remove(id);
      return Future.error(const AgentIMError('bad_request', 'not a command'));
    }
    return p.future.timeout(
      _o.commandTimeout,
      onTimeout: () {
        _pending.remove(id);
        _inOrder.remove(id);
        throw const AgentIMError('timeout', 'no answer from the client core', retryable: true);
      },
    );
  }

  /// Events: operation, side, signal, reset, storage, event, read, mute (and the ones above).
  /// Returns a function that stops it.
  void Function() on(String type, void Function(Map<String, dynamic>) fn) {
    (_listeners[type] ??= []).add(fn);
    return () => _listeners[type]?.remove(fn);
  }

  /// The platform's network: call on every change (connectivity_plus).
  void setNetwork(bool online) {
    if (_ptr != nullptr) agentim_client_network(_ptr, online ? 1 : 0, ++_generation);
  }

  /// The app came to the foreground or went to the background (AppLifecycleListener).
  void setForeground(bool foreground) {
    if (_ptr != nullptr) agentim_client_foreground(_ptr, foreground ? 1 : 0);
  }

  /// The conversation's messages, kept up to date.
  Messages messages(String convId) {
    final existing = _views[convId];
    if (existing != null) return existing;
    final v = _views[convId] = Messages._(this, convId);
    _command({'cmd': 'messages', 'conv_id': convId}, (r) {
      final m = r as Map;
      v._reset(m['messages'] as List?, m['has_older'] as bool?, m['older_end'] as String?);
    }).ignore();
    return v;
  }

  Future<void> connect() => command({'cmd': 'connect'});

  /// Closes the connection (the client stays usable; [dispose] frees it).
  Future<void> disconnect() => command({'cmd': 'close'});

  /// Logs out here: closes and wipes the cache and the queue (GDPR).
  Future<void> logout() => command({'cmd': 'logout'});

  /// Returns `{client_msg_id, key}` once the message is queued and shown.
  Future<Map<String, dynamic>> send(String convId, String text, {String? clientMsgId, String? replyTo}) async =>
      ((await command({
                'cmd': 'send',
                'conv_id': convId,
                'text': text,
                if (clientMsgId != null) 'client_msg_id': clientMsgId,
                if (replyTo != null) 'reply_to': replyTo,
              }))
              as Map)
          .cast<String, dynamic>();

  Future<void> edit(String convId, String msgId, String text) => command({'cmd': 'edit', 'conv_id': convId, 'msg_id': msgId, 'text': text});
  Future<void> recall(String convId, String msgId) => command({'cmd': 'recall', 'conv_id': convId, 'msg_id': msgId});
  Future<void> read(String convId, {int? seq}) => command({'cmd': 'read', 'conv_id': convId, if (seq != null) 'seq': seq});
  Future<void> retry(String convId, String clientMsgId) => command({'cmd': 'retry', 'conv_id': convId, 'client_msg_id': clientMsgId});
  Future<void> discard(String convId, String clientMsgId) => command({'cmd': 'discard', 'conv_id': convId, 'client_msg_id': clientMsgId});

  /// A request that needs the server now (approvals, decisions, queries).
  Future<Object?> call(String op, [Map<String, Object?> args = const {}]) => command({'cmd': 'call', 'op': op, 'args': args});
  Future<Object?> debugState() => command({'cmd': 'debug_state'});

  /// Stops the client; no event is handled after it returns. Waiting calls fail with `closed`.
  void dispose() {
    if (_ptr == nullptr) return;
    final c = _ptr;
    _ptr = nullptr;
    agentim_client_free(c);
    // Events already queued for this isolate are freed as they arrive, then the callback goes.
    Timer(const Duration(seconds: 1), _callable.close);
    for (final p in _pending.values) {
      p.completeError(const AgentIMError('closed', 'the client is closed'));
    }
    _pending.clear();
    _inOrder.clear();
  }
}
