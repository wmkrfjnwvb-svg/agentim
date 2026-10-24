// Unity WebGL bridge (docs/12 §2.12): the C ABI of the native runtime
// (agentim.h), implemented in the browser over @agentim/client-web, so the
// C# wrapper is the same on every platform. The page loads the browser SDK
// first and puts it in `window.AgentIMWeb` (see README).
//
// Events go to the C# callback as NUL-terminated UTF-8 on the main thread,
// in the order the client gives them; the string is freed after the call.

var AgentIMBridge = {
  $AIM: {
    clients: {},
    next: 0,
    str: function (ptr) {
      return ptr ? UTF8ToString(ptr) : null;
    },
    // Hands one event to C#: (ctx, json) as the native callback.
    emit: function (c, e) {
      if (!c.alive) return;
      var s = JSON.stringify(e);
      var n = lengthBytesUTF8(s) + 1;
      var p = _malloc(n);
      stringToUTF8(s, p, n);
      try {
        {{{ makeDynCall('vii', 'c.cb') }}}(c.ctx, p);
      } finally {
        _free(p);
      }
    },
  },

  agentim_client_new__deps: ['$AIM'],
  agentim_client_new: function (configPtr, storePtr, cb, ctx) {
    var web = (typeof window !== 'undefined' ? window : globalThis).AgentIMWeb;
    var cfg;
    try {
      cfg = JSON.parse(AIM.str(configPtr));
    } catch (e) {
      return 0;
    }
    if (!web || !cfg || !cfg.url || !cfg.device_id) return 0;
    var h = ++AIM.next;
    var c = { cb: cb, ctx: ctx, alive: true, client: null, queue: [], tokenWait: null };
    AIM.clients[h] = c;
    var store = AIM.str(storePtr);
    var run = function (id, cmd) {
      c.client.command(cmd).then(
        function (r) { AIM.emit(c, { type: 'reply', id: id, ok: true, result: r === undefined ? null : r }); },
        function (err) {
          AIM.emit(c, {
            type: 'reply', id: id, ok: false,
            error: { code: err && err.code ? err.code : 'internal', message: err && err.message ? err.message : String(err), retryable: !!(err && err.retryable) },
          });
        }
      );
    };
    c.run = run;
    web.AgentIMClient.create({
      url: cfg.url,
      deviceId: cfg.device_id,
      deviceName: cfg.device_name || undefined,
      platform: cfg.platform || 'unity-webgl',
      clientName: cfg.client_name || 'agentim-unity-webgl',
      clientVersion: cfg.client_version || '0.1.0',
      // A store name keeps the cache in IndexedDB; none keeps it in memory.
      store: store ? 'indexeddb' : 'memory',
      storeName: store || undefined,
      // C# answers need_token with agentim_client_token.
      tokenProvider: function () {
        return new Promise(function (ok, no) {
          c.tokenWait = { ok: ok, no: no };
          AIM.emit(c, { type: 'need_token' });
        });
      },
    }).then(function (client) {
      if (!c.alive) {
        client.dispose();
        return;
      }
      c.client = client;
      ['connection', 'conversations', 'messages', 'operation', 'side', 'signal', 'reset', 'storage', 'event', 'read', 'mute'].forEach(function (t) {
        client.on(t, function (e) { AIM.emit(c, e); });
      });
      var q = c.queue;
      c.queue = [];
      q.forEach(function (x) { run(x[0], x[1]); });
    }, function (err) {
      AIM.emit(c, { type: 'connection', state: 'failed', reason: 'unavailable' });
    });
    return h;
  },

  agentim_client_command__deps: ['$AIM'],
  agentim_client_command: function (h, id, cmdPtr) {
    var c = AIM.clients[h];
    if (!c) return -1;
    var cmd;
    try {
      cmd = JSON.parse(AIM.str(cmdPtr));
    } catch (e) {
      return -1;
    }
    if (!cmd || typeof cmd !== 'object' || Array.isArray(cmd)) return -1;
    if (c.client) c.run(id, cmd);
    else c.queue.push([id, cmd]);
    return 0;
  },

  agentim_client_token__deps: ['$AIM'],
  agentim_client_token: function (h, tokenPtr, refused) {
    var c = AIM.clients[h];
    if (!c || !c.tokenWait) return;
    var w = c.tokenWait;
    c.tokenWait = null;
    var t = AIM.str(tokenPtr);
    if (t) w.ok(t);
    else {
      var e = new Error(refused ? 'refused' : 'token failed');
      if (refused) {
        var web = (typeof window !== 'undefined' ? window : globalThis).AgentIMWeb;
        e = new web.TokenRefused('refused');
      }
      w.no(e);
    }
  },

  // The browser's own online and visibility signals already reach the
  // client; these are accepted so the C# code is the same everywhere.
  agentim_client_network: function (h, online, generation) {},
  agentim_client_foreground: function (h, foreground) {},

  agentim_client_free__deps: ['$AIM'],
  agentim_client_free: function (h) {
    var c = AIM.clients[h];
    if (!c) return;
    c.alive = false;
    delete AIM.clients[h];
    if (c.client) c.client.dispose();
  },

  agentim_version__deps: ['$AIM'],
  agentim_version: function () {
    if (!AIM.version) {
      var s = '0.1.0-webgl';
      var n = lengthBytesUTF8(s) + 1;
      AIM.version = _malloc(n);
      stringToUTF8(s, AIM.version, n);
    }
    return AIM.version;
  },
};

autoAddDeps(AgentIMBridge, '$AIM');
mergeInto(LibraryManager.library, AgentIMBridge);
