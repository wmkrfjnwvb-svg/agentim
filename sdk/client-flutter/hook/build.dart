// Provides the Rust core (sdk/client-rust) to Dart and Flutter apps as a
// code asset (docs/12 §2.12.1), for the platform being built:
//
// - From the published package: the library built for the release is
//   downloaded from `hook/prebuilt.json`'s base URL and checked against the
//   SHA-256 recorded there; a mismatch fails the build.
// - In this repository (no prebuilt.json): cargo builds it from
//   ../client-rust (cross targets need their linker configured in cargo).
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:code_assets/code_assets.dart';
import 'package:crypto/crypto.dart';
import 'package:hooks/hooks.dart';

void main(List<String> args) async {
  await build(args, (input, output) async {
    if (!input.config.buildCodeAssets) return;
    final code = input.config.code;
    final key = _key(code);
    final manifest = File.fromUri(input.packageRoot.resolve('hook/prebuilt.json'));
    final Uri file;
    if (manifest.existsSync()) {
      output.dependencies.add(manifest.uri);
      file = await _download(input, manifest, key, code.targetOS.dylibFileName('agentim_client'));
    } else {
      file = await _cargo(input, output, code);
    }
    output.assets.code.add(CodeAsset(package: input.packageName, name: 'agentim_client.dart', linkMode: DynamicLoadingBundled(), file: file));
  });
}

/// The release file key: `<os>-<arch>`, `ios-sim-<arch>` for the simulator.
String _key(CodeConfig code) {
  final os = code.targetOS;
  final sim = os == OS.iOS && code.iOS.targetSdk == IOSSdk.iPhoneSimulator;
  return '${sim ? 'ios-sim' : os.name}-${code.targetArchitecture.name}';
}

Future<Uri> _download(BuildInput input, File manifest, String key, String libName) async {
  final m = jsonDecode(manifest.readAsStringSync()) as Map;
  final entry = (m['files'] as Map)[key] as Map?;
  if (entry == null) throw UnsupportedError('agentim_client has no prebuilt library for $key');
  final name = entry['file'] as String;
  final sha = entry['sha256'] as String;
  final dir = Directory.fromUri(input.outputDirectoryShared.resolve('$sha/'))..createSync(recursive: true);
  final dest = File.fromUri(dir.uri.resolve(libName));
  if (dest.existsSync() && sha256.convert(dest.readAsBytesSync()).toString() == sha) return dest.uri;
  final url = Uri.parse(m['base_url'] as String).resolve(name);
  final List<int> bytes;
  if (url.scheme == 'file') {
    bytes = File.fromUri(url).readAsBytesSync();
  } else {
    final http = HttpClient();
    try {
      final res = await (await http.getUrl(url)).close();
      if (res.statusCode != 200) throw HttpException('HTTP ${res.statusCode}', uri: url);
      bytes = (await res.fold<BytesBuilder>(BytesBuilder(copy: false), (b, d) => b..add(d))).takeBytes();
    } finally {
      http.close(force: true);
    }
  }
  final got = sha256.convert(bytes).toString();
  if (got != sha) throw StateError('agentim_client: $url has SHA-256 $got, expected $sha');
  final part = File('${dest.path}.part')..writeAsBytesSync(bytes, flush: true);
  part.renameSync(dest.path);
  return dest.uri;
}

Future<Uri> _cargo(BuildInput input, BuildOutputBuilder output, CodeConfig code) async {
  final ws = input.packageRoot.resolve('../client-rust/');
  if (!File.fromUri(ws.resolve('Cargo.toml')).existsSync()) {
    throw StateError('agentim_client: no hook/prebuilt.json and no ../client-rust to build from');
  }
  final triple = _triple(code);
  final host = code.targetOS == OS.current && code.targetArchitecture == Architecture.current;
  final args = [
    'build',
    '--locked',
    '-p',
    'agentim-client',
    if (!host) ...['--target', triple],
  ];
  final r = await Process.run('cargo', args, workingDirectory: ws.toFilePath(), runInShell: Platform.isWindows);
  if (r.exitCode != 0) throw ProcessException('cargo', args, '${r.stdout}${r.stderr}', r.exitCode);
  for (final d in ['core/src/', 'native/src/']) {
    for (final f in Directory.fromUri(ws.resolve(d)).listSync(recursive: true)) {
      if (f is File) output.dependencies.add(f.uri);
    }
  }
  output.dependencies.addAll([ws.resolve('Cargo.lock'), ws.resolve('Cargo.toml'), ws.resolve('core/Cargo.toml'), ws.resolve('native/Cargo.toml')]);
  return ws.resolve('target/${host ? '' : '$triple/'}debug/${code.targetOS.dylibFileName('agentim_client')}');
}

String _triple(CodeConfig code) {
  final arch = code.targetArchitecture;
  return switch (code.targetOS) {
    OS.android => switch (arch) {
      Architecture.arm64 => 'aarch64-linux-android',
      Architecture.arm => 'armv7-linux-androideabi',
      Architecture.x64 => 'x86_64-linux-android',
      Architecture.ia32 => 'i686-linux-android',
      _ => throw UnsupportedError('android $arch'),
    },
    OS.iOS => switch ((arch, code.iOS.targetSdk == IOSSdk.iPhoneSimulator)) {
      (Architecture.arm64, false) => 'aarch64-apple-ios',
      (Architecture.arm64, true) => 'aarch64-apple-ios-sim',
      (Architecture.x64, true) => 'x86_64-apple-ios',
      _ => throw UnsupportedError('ios $arch'),
    },
    OS.macOS => '${arch == Architecture.arm64 ? 'aarch64' : 'x86_64'}-apple-darwin',
    OS.linux => '${arch == Architecture.arm64 ? 'aarch64' : 'x86_64'}-unknown-linux-gnu',
    OS.windows => '${arch == Architecture.arm64 ? 'aarch64' : 'x86_64'}-pc-windows-msvc',
    _ => throw UnsupportedError('${code.targetOS}'),
  };
}
