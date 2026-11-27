# Compatibility patch

Upstream: https://github.com/Stebalien/xattr
Revision: e42c1da499b72103a9c9ef5693ff31f998ad31e6 (release 0.2.3).
License: MIT OR Apache-2.0; original notices retained.

GPUI 0.2.2 depends on this release through zed-async-tar. Modern libc no longer
exports the deprecated ENOATTR alias on Linux/Android. Use ENODATA on those
platforms, which is the same error number and the correct missing-attribute
constant; retain ENOATTR on the BSD/macOS platforms. No other source changes.

Remove this patch when the frontend dependency no longer needs xattr 0.2.x.
It is excluded from workspace membership because it is third-party code.
