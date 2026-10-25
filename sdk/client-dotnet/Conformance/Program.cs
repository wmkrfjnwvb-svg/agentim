// The wrapper conformance scenario (docs/12 §2.13), against a real `agentim`:
// the same steps every platform wrapper runs.
//
//   AGENTIM_BIN=…/agentim AGENTIM_LIB=…/target/debug dotnet run --project Conformance

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Net.Http;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json.Nodes;
using System.Threading.Tasks;
using AgentIM.Client;

var root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../.."));
var exe = OperatingSystem.IsWindows() ? ".exe" : "";
var bin = Environment.GetEnvironmentVariable("AGENTIM_BIN") ?? Path.Combine(root, "server/target/debug/agentim" + exe);
#if !AGENTIM_PACKAGE
// Built against the project: the library from the local Rust build. Built
// against the NuGet package (-p:AgentIMPackage=<version>), .NET loads the
// one the package ships for this platform.
var libDir = Environment.GetEnvironmentVariable("AGENTIM_LIB") ?? Path.Combine(root, "sdk/client-rust/target/debug");
NativeLibrary.SetDllImportResolver(typeof(AgentIMClient).Assembly, (name, asm, path) =>
{
    var file = OperatingSystem.IsWindows() ? "agentim_client.dll" : OperatingSystem.IsMacOS() ? "libagentim_client.dylib" : "libagentim_client.so";
    return NativeLibrary.Load(Path.Combine(libDir, file));
});
#endif

var dir = Directory.CreateTempSubdirectory("agentim-dotnet-").FullName;
var port = 19800 + Random.Shared.Next(150);
var url = $"http://127.0.0.1:{port}";
var server = Process.Start(new ProcessStartInfo(bin, $"--demo --db \"{Path.Combine(dir, "a.db")}\" --addr 127.0.0.1:{port}")
{
    RedirectStandardOutput = true,
    RedirectStandardError = true,
})!;
server.OutputDataReceived += (_, _) => { };
server.ErrorDataReceived += (_, _) => { };
server.BeginOutputReadLine();
server.BeginErrorReadLine();
var http = new HttpClient();
var failures = 0;
var n = 0;

void Check(bool ok, string what)
{
    Console.WriteLine($"{(ok ? "ok  " : "FAIL")} {what}");
    if (!ok) failures++;
}

// B12: a reset replaces a view in one update, never empty in between.
{
    var v = new Messages(null!, "c1");
    JsonObject Up(string key, double order) => new JsonObject { ["op"] = "upsert", ["message"] = new JsonObject { ["key"] = key, ["order"] = order } };
    v.Apply(new JsonArray(Up("a", 1), Up("b", 2)));
    var lists = new List<string>();
    using (v.List.Subscribe(l => lists.Add(string.Join(",", l.Select(m => (string)m["key"]!)))))
    {
        lists.Clear();
        v.Apply(new JsonArray(Up("b", 2), Up("c", 3)), reset: true);
    }
    Check(lists.Count == 1 && lists[0] == "b,c", $"B12 a reset is one update ({string.Join(" | ", lists)})");
}

async Task<bool> Until(Func<bool> f, int ms = 20000)
{
    var end = DateTime.UtcNow.AddMilliseconds(ms);
    while (DateTime.UtcNow < end)
    {
        if (f()) return true;
        await Task.Delay(25);
    }
    return f();
}

async Task<bool> UntilAsync(Func<Task<bool>> f, int ms = 20000)
{
    var end = DateTime.UtcNow.AddMilliseconds(ms);
    while (DateTime.UtcNow < end)
    {
        if (await f()) return true;
        await Task.Delay(50);
    }
    return await f();
}

async Task<(string Id, string Name, string Token)> Person(string name)
{
    name = $"{name}{++n}";
    var req = new HttpRequestMessage(HttpMethod.Post, $"{url}/api/dev/users") { Content = new StringContent($"{{\"name\":\"{name}\"}}", Encoding.UTF8, "application/json") };
    req.Headers.Add("Origin", url);
    var r = JsonNode.Parse(await (await http.SendAsync(req)).Content.ReadAsStringAsync())!;
    return ((string)r["user"]!["id"]!, name, (string)r["token"]!);
}

AgentIMClient Client((string Id, string Name, string Token) p, string? store = null, string? device = null, Func<Task<string>>? token = null) =>
    AgentIMClient.Create(new ClientOptions
    {
        Url = url,
        DeviceId = device ?? $"{p.Name}-d",
        StoreDir = store,
        TokenProvider = _ => token != null ? token() : Task.FromResult(p.Token),
    });

string[] Sent(Messages v) => v.List.Value.Where(m => (string?)m["status"] == "sent").Select(m => (string)m["content"]!).ToArray();

try
{
    for (var i = 0; i < 300 && !await http.GetAsync($"{url}/readyz").ContinueWith(t => t.Status == TaskStatus.RanToCompletion && t.Result.IsSuccessStatusCode); i++)
        await Task.Delay(100);
    Check(AgentIMClient.Version.Length > 0, $"version {AgentIMClient.Version}");

    var alice = await Person("alice");
    var bob = await Person("bob");
    using var a = Client(alice);
    using var b = Client(bob);
    await Task.WhenAll(a.ConnectAsync(), b.ConnectAsync());
    Check(await Until(() => a.Connection.Value.State == "connected" && b.Connection.Value.State == "connected"),
        $"W1 both connect ({a.Connection.Value.State} {a.Connection.Value.Reason}, {b.Connection.Value.State} {b.Connection.Value.Reason})");

    var conv = (string)(await a.CallAsync("create_conv", new JsonObject { ["kind"] = "group", ["title"] = "dotnet", ["members"] = new JsonArray(bob.Id) }))!["conv_id"]!;
    var av = a.Messages(conv);
    var sent = await a.SendAsync(conv, "hello from C# 👋");
    Check(((string)sent!["key"]!).StartsWith("local:") && av.List.Value.Any(m => (string?)m["content"] == "hello from C# 👋"), "W2 a send is shown when it returns");
    Check(await Until(() => Sent(av).Contains("hello from C# 👋")), "W2 and then sent (emoji intact)");
    var bv = b.Messages(conv);
    Check(await Until(() => Sent(bv).Contains("hello from C# 👋")), "W2 the other side sees it");
    Check(await Until(() => b.Conversations.Value.Any(c => (string?)c["conv_id"] == conv)), "W2 and has the conversation in its list");

    var mine = av.List.Value.First(m => (string?)m["content"] == "hello from C# 👋");
    await a.EditAsync(conv, (string)mine["msg_id"]!, "hello, edited");
    Check(await Until(() => Sent(bv).FirstOrDefault() == "hello, edited"), "W3 an edit reaches the other side");
    await a.RecallAsync(conv, (string)mine["msg_id"]!);
    Check(await Until(() => (string?)bv.List.Value.FirstOrDefault()?["state"] == "recalled"), "W3 a recall reaches the other side");

    var ops = new System.Collections.Concurrent.ConcurrentBag<string>();
    using (a.On("operation", e => ops.Add((string?)e["result"] ?? "")))
    {
        await a.SendAsync("c_not_mine", "nope", clientMsgId: "bad1");
        var nv = a.Messages("c_not_mine");
        Check(await Until(() => nv.List.Value.Any(m => (string?)m["status"] == "failed") && ops.Contains("failed")), "W4 a refused send fails and is reported");
        await a.DiscardAsync("c_not_mine", "bad1");
        Check(await Until(() => nv.List.Value.Count == 0), "W4 discard removes it");
    }

    var code = "";
    try { await a.CallAsync("get_request", new JsonObject { ["request_id"] = "nope" }); }
    catch (AgentIMException e) { code = e.Code; }
    Check(code == "not_found", $"W5 an online call answers with the server's code ({code})");
    code = "";
    try { await a.CommandAsync(new JsonObject { ["cmd"] = "no_such_command" }); }
    catch (AgentIMException e) { code = e.Code; }
    Check(code == "bad_request", $"W5 an unknown command is bad_request ({code})");

    // The queue survives a restart and is sent once.
    var store = Path.Combine(dir, "alice-cache");
    using (var q = Client(alice, store, "alice-q"))
    {
        foreach (var t in new[] { "one", "two", "three" }) await q.SendAsync(conv, t);
    }
    using (var q = Client(alice, store, "alice-q"))
    {
        var v = q.Messages(conv);
        Check(await Until(() => v.List.Value.Count(m => (string?)m["status"] == "sending") == 3), "W6 after a restart the queue shows before connecting");
        await q.ConnectAsync();
        Check(await Until(() => string.Join("|", Sent(bv).Where(t => t is "one" or "two" or "three")) == "one|two|three"), "W6 and is sent once, in order");
        Check(await UntilAsync(async () => ((JsonArray)(await q.CommandAsync(new JsonObject { ["cmd"] = "pending" }))!["ops"]!).Count == 0), "W6 and the queue is empty");

        // Offline: queued; back online: sent once.
        q.SetNetwork(false);
        Check(await Until(() => q.Connection.Value.State != "connected"), $"W7 offline is not connected ({q.Connection.Value.State})");
        await q.SendAsync(conv, "while offline");
        q.SetNetwork(true);
        Check(await Until(() => Sent(bv).Count(t => t == "while offline") == 1), "W7 back online it is sent once");
    }

    using var r = Client(alice, device: "alice-r", token: () => throw new TokenRefusedException("no"));
    await r.ConnectAsync();
    Check(await Until(() => r.Connection.Value.State == "failed"), "W8 a refused token stops the client");
    Check(r.Connection.Value.Reason == "unauthenticated", $"W8 as unauthenticated ({r.Connection.Value.Reason})");

    var d = Client(alice, device: "alice-d");
    d.Dispose();
    d.Dispose();
    code = "";
    try { await d.ConnectAsync(); }
    catch (AgentIMException e) { code = e.Code; }
    Check(code == "closed", "W9 after dispose, commands fail with closed (dispose twice is fine)");
}
catch (Exception e)
{
    Console.WriteLine($"FAIL {e}");
    failures++;
}
finally
{
    server.Kill();
    try { Directory.Delete(dir, true); } catch { }
}
return failures == 0 ? 0 : 1;
