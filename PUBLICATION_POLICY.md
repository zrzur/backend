# Public source publication policy

Allowed after review: application source, package/Cargo manifests and lockfiles, required build tools, actual CI workflows, synthetic tests/fixtures, product assets with provenance, public developer documentation, licenses and attributions.

Never publish: passwords/tokens/private keys, instance .env/config files, credentials, database contents/dumps/volumes, logs/HARs, screenshots containing user data, node_modules/build outputs/installers, local live-test/seed/demo tools, rejected design galleries or inactive copied release workflows. Example configurations must contain placeholders or explicitly local demo values only. Preserve required upstream copyright and licenses.

PUBLIC_SOURCE_MANIFEST.json lists the reviewed files, hashes and dependency pins. Run python scripts/publication_guard.py to verify; --write only produces a candidate manifest and requires review. CI never silently approves/regenerates it. Tests/fixtures are synthetic product test inputs, not user database records.

The guard checks the current source tree, not old Git commits. Removing a file from main does not remove it from history. Historical remediation requires a separately reviewed plan and explicit authorization for any protected history change. Do not delete or overwrite working data.
