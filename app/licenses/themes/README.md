# Theme reconstruction sources

`notices.json` identifies the licensed structural sources and their exact hashes.
The license generator includes their original notices in the desktop inventory.

The Codex light/dark token definitions derive from the MIT-licensed Pierre
0.0.29 templates at commit `c5420e65d638547a94ad7403e0f6becc4d40d02c`.
The Raycast light/dark token definitions derive from Raycast's MIT-licensed
ray.so CSS-variable template at commit
`2e6a32808fb13a9246cac153e53e729def5ea7e8`, with explicit selector/style edits.
The retained ray.so source credits its adaptation from Shiki. Shiki notices
are also included in the client dependency inventory.

Existing appearance parameters preserve the previous colors, fonts, and chrome.
These source notices do not assign a license or authorship to that configuration.
Sixteen other local theme files retain unresolved complete upstream provenance;
they remain unchanged and receive no new license or authorship claim.

The exact files, baseline hashes, derivation limits, and seed comparison are in
`app/publication/custom-theme-provenance.json`. From the repository root:

```sh
node app/publication/verify-theme-reconstruction.mjs
node app/scripts/generate-code-theme-seeds.mjs --check
node app/scripts/generate-client-licenses.mjs --check
```
