# Client third-party notices

The repository license does not replace the licenses of dependencies or bundled
third-party assets. Original notice text is retained here and in installed
packages. `dependency-notices.json` identifies the exact versions and hashes of
supplemental notices and the hand landmark model. `icon-assets/notices.json`
lists the icons taken from open-source sets and the notices that cover them.

From `app/`, regenerate the in-app notice inventory with:

```sh
npm run gen:licenses
node scripts/generate-client-licenses.mjs --check
```

The generator verifies required installed package versions against the lockfile,
checks supplemental notice and asset hashes, and reports absent optional
dependencies. It does not silently omit a missing required dependency.

The Octopus logo, video, and agent illustrations are the project's own artwork
and are not attributed to a third party.
