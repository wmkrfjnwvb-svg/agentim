"""Writes the .meta file Unity needs next to every file and folder of the
package (docs/12 §2.12.1): an installed package is read-only, so Unity
cannot make them itself. GUIDs are derived from the path (the same on
every build); native plugins get the platform and CPU they are for.

    python3 meta.py <package dir>        (writes, then checks)
    python3 meta.py --check <package dir>
"""
import hashlib
import pathlib
import sys

# Plugins/<dir> -> (platform entries enabled, settings); everything else off.
PLUGINS = {
    "Android/arm64-v8a": ("Android", {"CPU": "ARM64"}),
    "Android/armeabi-v7a": ("Android", {"CPU": "ARMv7"}),
    "Android/x86_64": ("Android", {"CPU": "X86_64"}),
    "iOS": ("iOS", {"AddToEmbeddedBinaries": "false", "CPU": "AnyCPU", "CompileFlags": "", "FrameworkDependencies": "Security;SystemConfiguration;"}),
    "macOS": ("OSXUniversal", {"CPU": "AnyCPU"}),
    "Windows/x86_64": ("Win64", {"CPU": "x86_64"}),
    "WebGL": ("WebGL", {}),
}
EDITORS = {"macOS": ("OSX", "AnyCPU"), "Windows/x86_64": ("Windows", "x86_64")}
NATIVE = (".so", ".a", ".dylib", ".dll", ".jslib")


def guid(rel: str) -> str:
    return hashlib.md5(f"com.agentim.client/{rel}".encode()).hexdigest()


def plugin(rel: str) -> str:
    where = rel.split("Plugins/", 1)[1].rsplit("/", 1)[0]
    platform, settings = PLUGINS[where]
    rows = [("Any", "", False, {}), ("Editor", "Editor", where in EDITORS, {"CPU": EDITORS[where][1], "OS": EDITORS[where][0]} if where in EDITORS else {"CPU": "AnyCPU", "DefaultValueInitialized": "true", "OS": "AnyOS"})]
    for p in ("Android", "iOS", "OSXUniversal", "Win64", "WebGL"):
        rows.append((p, p, p == platform, settings if p == platform else {}))
    out = []
    for first_key, first_val, enabled, s in rows:
        out.append(f"  - first:\n      {first_key}: {first_val}\n    second:\n      enabled: {1 if enabled else 0}\n")
        out.append("      settings: {}\n" if not s else "      settings:\n" + "".join(f"        {k}: {v}\n" for k, v in sorted(s.items())))
    return (
        "PluginImporter:\n  externalObjects: {}\n  serializedVersion: 2\n  iconMap: {}\n  executionOrder: {}\n"
        "  defineConstraints: []\n  isPreloaded: 0\n  isOverridable: 0\n  isExplicitlyReferenced: 0\n"
        "  validateReferences: 1\n  platformData:\n" + "".join(out) + "  userData: \n  assetBundleName: \n  assetBundleVariant: \n"
    )


def meta(path: pathlib.Path, rel: str) -> str:
    head = f"fileFormatVersion: 2\nguid: {guid(rel)}\n"
    if path.is_dir():
        return head + "folderAsset: yes\nDefaultImporter:\n  externalObjects: {}\n  userData: \n  assetBundleName: \n  assetBundleVariant: \n"
    if rel.endswith(NATIVE):
        return head + plugin(rel)
    if rel == "package.json":
        return head + "PackageManifestImporter:\n  externalObjects: {}\n  userData: \n  assetBundleName: \n  assetBundleVariant: \n"
    if rel.endswith(".cs"):
        return head + "MonoImporter:\n  externalObjects: {}\n  serializedVersion: 2\n  defaultReferences: []\n  executionOrder: 0\n  icon: {instanceID: 0}\n  userData: \n  assetBundleName: \n  assetBundleVariant: \n"
    if rel.endswith(".asmdef"):
        return head + "AssemblyDefinitionImporter:\n  externalObjects: {}\n  userData: \n  assetBundleName: \n  assetBundleVariant: \n"
    return head + "TextScriptImporter:\n  externalObjects: {}\n  userData: \n  assetBundleName: \n  assetBundleVariant: \n"


def entries(root: pathlib.Path):
    for p in sorted(root.rglob("*")):
        rel = p.relative_to(root).as_posix()
        if not rel.endswith(".meta"):
            yield p, rel


def check(root: pathlib.Path) -> int:
    bad, seen = [], {}
    for p, rel in entries(root):
        m = p.with_name(p.name + ".meta")
        if not m.exists():
            bad.append(f"no meta: {rel}")
            continue
        g = next((l.split(": ")[1] for l in m.read_text().splitlines() if l.startswith("guid: ")), None)
        if not g or len(g) != 32 or g in seen:
            bad.append(f"bad or repeated guid in {rel}.meta (also {seen.get(g)})")
        seen[g] = rel
        if rel.endswith(NATIVE) and "enabled: 1" not in m.read_text():
            bad.append(f"plugin enabled for no platform: {rel}")
    for m in root.rglob("*.meta"):
        if not m.with_name(m.name[: -len(".meta")]).exists():
            bad.append(f"meta without its asset: {m.relative_to(root)}")
    print("\n".join(bad) or f"{len(seen)} assets, each with its meta")
    return 1 if bad else 0


if __name__ == "__main__":
    only_check = sys.argv[1] == "--check"
    root = pathlib.Path(sys.argv[-1])
    if not only_check:
        for p, rel in entries(root):
            p.with_name(p.name + ".meta").write_text(meta(p, rel))
    sys.exit(check(root))
