#!/usr/bin/env python3
"""Check an explicitly reviewed public source manifest; never print matched secrets."""
import argparse, hashlib, json, pathlib, re, subprocess, sys
ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST = ROOT / 'PUBLIC_SOURCE_MANIFEST.json'
DENY_PARTS = {'.git', 'node_modules', '.data', 'docs-private', 'dist', 'dist-release-audit', 'out', 'coverage', 'test-results', 'playwright-report', '.venv', '__pycache__', 'legacy-workflows', 'workflows.zarzour-disabled'}
SENSITIVE_SUFFIXES = ('.pfx', '.p12', '.pem', '.key', '.sqlite', '.sqlite3', '.dump', '.bak', '.bundle', '.log', '.har', '.tgz')
SIGNATURES = [('private-key', re.compile(rb'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----')), ('github-token', re.compile(rb'\b(?:github_pat_[A-Za-z0-9_]{30,}|gh[pousr]_[A-Za-z0-9]{30,})\b')), ('aws-key', re.compile(rb'\bAKIA[0-9A-Z]{16}\b'))]
REVIEWED_LICENSE_LINKS = {'crates/bonfire/LICENSE', 'crates/delta/LICENSE'}

def category(name):
    if name.startswith(('.github/', 'scripts/', '.mise/')): return 'reviewed-build-and-ci'
    if 'LICENSE' in name.upper() or 'ATTRIBUTION' in name.upper() or 'NOTICE' in name.upper(): return 'license-and-attribution'
    if '/tests/' in name or '/test/' in name or '/e2e/' in name or '.test.' in name or '.spec.' in name: return 'synthetic-test-source-or-fixture'
    if '/public/' in name or name.startswith(('assets/', 'brand/')): return 'reviewed-product-asset'
    if name.endswith(('.md', '.rst')) or name.startswith(('doc/', 'docs/')): return 'public-documentation'
    return 'product-source-or-build-manifest'

def forbidden(name):
    p = pathlib.PurePosixPath(name)
    if any(part in DENY_PARTS for part in p.parts): return 'private-or-generated-directory'
    if p.name.startswith('.env') and not p.name.endswith('.example'): return 'environment-file'
    if p.name in {'secrets.env', 'credentials.json', 'service-account.json', 'PUBLICATION_AUDIT.json'}: return 'credential-or-private-report'
    if name in {'Revolt.toml', 'livekit.yml', 'stoat.json'}: return 'instance-local-configuration'
    if name.endswith(SENSITIVE_SUFFIXES): return 'secret-or-runtime-artifact-extension'
    if '/public/badges/zarzour/' in name or 'community-demo' in p.name or p.name.endswith('-live.mjs'): return 'private-prototype-or-live-test-harness'
    return None

def collect():
    raw = subprocess.check_output(['git', 'ls-files', '--stage', '-z'], cwd=ROOT)
    entries = []
    for entry in raw.split(b'\0'):
        if not entry: continue
        metadata, name = entry.decode('utf-8').split('\t', 1)
        mode, oid, stage = metadata.split()
        if stage != '0': raise RuntimeError('Unresolved index conflict')
        entries.append((name, mode, oid))
    entries.sort()
    blobs = [(name, mode, oid) for name, mode, oid in entries if mode != '160000' and name != MANIFEST.name]
    # Hash canonical Git bytes, not Windows CRLF working copies. CI sees the same bytes.
    packed = subprocess.check_output(['git', 'cat-file', '--batch'], input=''.join(oid + '\n' for _, _, oid in blobs).encode(), cwd=ROOT)
    offset = 0
    files, errors = [], []
    for name, mode, oid in blobs:
        end = packed.index(b'\n', offset)
        size = int(packed[offset:end].split()[2])
        data = packed[end + 1:end + 1 + size]
        offset = end + size + 2
        target = ROOT / name
        if not target.exists(): errors.append((name, 'tracked-file-missing')); continue
        reason = forbidden(name)
        if reason: errors.append((name, reason)); continue
        if mode == '120000' or target.is_symlink():
            if not (name in REVIEWED_LICENSE_LINKS and data == b'../../LICENSE'):
                errors.append((name, 'symlink-needs-explicit-review')); continue
        if len(data) > 10 * 1024 * 1024: errors.append((name, 'oversized-file-needs-review')); continue
        for kind, pattern in SIGNATURES:
            if pattern.search(data): errors.append((name, kind))
        files.append({'path': name, 'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data), 'category': category(name)})
    pins = [{'path': name, 'commit': oid} for name, mode, oid in entries if mode == '160000']
    if subprocess.call(['git', 'diff', '--quiet', '--ignore-submodules=all', '--', '.', ':!PUBLIC_SOURCE_MANIFEST.json'], cwd=ROOT):
        errors.append(('[working tree]', 'unstaged-changes-must-be-reviewed'))
    return {'format': 1, 'scope': 'reviewed-current-source-tree-not-historical-secret-clearance', 'files': files, 'submodules': pins}, errors

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--write', action='store_true', help='Write a candidate manifest for explicit human review')
    args = parser.parse_args()
    actual, errors = collect()
    if errors:
        for name, kind in errors: print(f'BLOCKED {kind}: {name}')
        return 1
    if args.write:
        MANIFEST.write_text(json.dumps(actual, indent=2) + '\n', encoding='utf-8')
        print(f'Candidate manifest: {len(actual["files"])} files; review changes before publication.')
        return 0
    if not MANIFEST.exists(): print('BLOCKED missing reviewed manifest'); return 1
    expected = json.loads(MANIFEST.read_text(encoding='utf-8'))
    if expected != actual: print('BLOCKED source differs from reviewed manifest; review and regenerate explicitly.'); return 1
    print(f'PASS {len(actual["files"])} reviewed source files; known secret signatures absent. This is not proof of no secrets or release readiness.')
    return 0

if __name__ == '__main__': sys.exit(main())
