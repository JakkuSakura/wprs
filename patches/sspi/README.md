This directory contains local patchsets for third-party dependencies.

`sspi-0.16.1-wprs.patch` captures the delta between the crates.io `sspi` crate
version `0.16.1` and the historically vendored+patched snapshot that lived in
`vendor/sspi`.

Apply (from a checkout of the crates.io `sspi` 0.16.1 source tree):

`git apply /path/to/wprsx/patches/sspi/sspi-0.16.1-wprs.patch`

Notes:
- This repo now prefers using upstream crates.io `sspi` by default.
- Keep this patch around in case the same fixes are needed again (e.g. when
  debugging RDP/NLA/Kerberos issues).
