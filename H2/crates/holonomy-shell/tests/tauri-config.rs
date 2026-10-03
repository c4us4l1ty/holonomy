//! What `tauri.conf.json` has to say, checked as data.
//!
//! # Why this suite exists
//!
//! Because a file association is not code, so no compiler sees it, and because the claim it
//! makes is only true once an *installer* has run on a user's machine — a claim no test on
//! this machine can make. What this holds is the narrower and still-useful thing: the
//! declaration is present, internally consistent, and complete enough that a bundler has
//! everything it needs to register it.
//!
//! # What it explicitly does not claim
//!
//! That a `.holo` file opens when double-clicked. That requires the `.deb`/`.msi`/`.app` to
//! have been installed, which needs a packaging run per platform, and CI's smoke job builds
//! with `--no-bundle` because AppImage tooling fails for reasons unrelated to the app. The
//! parity between "declared here" and "registered on a user's machine" is therefore still
//! open, and `scripts/smoke-production.sh` prints that it is open rather than implying
//! otherwise.

use serde_json::Value;

/// The config, read from the source tree rather than from `generate_context!`.
///
/// Reading the file is deliberate. `tauri::generate_context!()` gives the *compiled* config,
/// which would agree with the file even if the file were edited after the build — and this
/// suite's whole value is catching an edit nobody rebuilt after. A generated context also
/// bakes in the whole schema, so reading the file keeps the test about the declaration.
fn config() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json");
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{path} is not valid JSON: {e}"))
}

fn extension() -> String {
    // The extension is defined once, in the crate that owns the format, so the association
    // cannot drift from the code that opens the files. `holo::FILE_EXTENSION` rather than a
    // literal here is the whole point of the check.
    holonomy_core::holo::FILE_EXTENSION.to_string()
}

/// The `--prefix` path a before-command passes to npm.
fn before_command_prefix(conf: &Value, key: &str) -> String {
    let cmd = conf["build"][key]
        .as_str()
        .unwrap_or_else(|| panic!("build.{key} is missing or not a string"));
    cmd.split_whitespace()
        .skip_while(|w| *w != "--prefix")
        .nth(1)
        .unwrap_or_else(|| {
            panic!("build.{key} does not pass --prefix to npm: `{cmd}`")
        })
        .to_string()
}

/// Normalise a relative path the way the filesystem would, without touching the disk.
///
/// Written out rather than using `Path::canonicalize` because the directories do not have to
/// exist for this to be meaningful — they *should* exist, but a test that fails with "no such
/// file" instead of "wrong number of `..`" sends the reader looking in the wrong place.
fn normalise(base: &str, relative: &str) -> String {
    // The leading slash is part of the path, not an empty segment to be filtered out —
    // dropping it turns an absolute path into a relative one, and then the check that the
    // directory exists is looking somewhere relative to the test's working directory.
    let absolute = base.starts_with('/');
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for segment in relative.split('/').filter(|s| !s.is_empty()) {
        match segment {
            "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if absolute {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

/// The directory `tauri.conf.json` itself lives in, as an absolute path.
fn tauri_dir() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

/// Drop the last `n` segments of a `/`-separated path, keeping it absolute if it was.
fn pop(path: &str, n: usize) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for _ in 0..n {
        parts.pop();
    }
    if absolute {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

#[test]
fn the_before_commands_resolve_to_the_frontend_package_from_where_tauri_runs_them() {
    // # The trap
    //
    // `beforeBuildCommand` and `frontendDist` look like they should be written the same way
    // and are not. Two different directories are the base for each:
    //
    // | key                  | resolved relative to                   |
    // |----------------------|----------------------------------------|
    // | `beforeBuildCommand` | the **parent** of the tauri directory |
    // | `frontendDist`       | the tauri directory itself            |
    //
    // That is not a subtlety invented here. Tauri runs the before-commands from the *app*
    // directory — the one holding the frontend's `package.json`, which in the standard layout
    // is the parent of `src-tauri`. It is caught in a way that reads like a path typo: on a
    // runner whose workspace is `/home/runner/work/holonomy/holonomy`, `../../app` resolved
    // to `/home/runner/work/holonomy/app` and the release smoke job died with
    //
    // ```text
    //   npm error enoent Could not read package.json:
    //     open '/home/runner/work/holonomy/app/package.json'
    // ```
    //
    // on a step named "cargo tauri build", before a single line of Rust or TypeScript ran.
    // Nothing in this file checked either path, which is why it survived a push.
    //
    // # What is asserted
    //
    // Two things, and the second is what makes the first worth anything:
    //
    // 1. The `--prefix` in each before-command lands on `app`, counted from the parent of the
    //    tauri directory, and `frontendDist` lands on `app/dist`, counted from the tauri
    //    directory.
    // 2. Both of those directories **exist and hold what they claim to hold** —
    //    `app/package.json` and `app/dist`. A path can be spelled correctly as a string and
    //    still point nowhere, and only the second half of this check would notice.
    //
    // Checked as text rather than by canonicalising, so the assertion is about the number of
    // levels rather than about what happens to be checked out where the test runs.
    // # Why the arithmetic is done in forward slashes
    //
    // `CARGO_MANIFEST_DIR` is `D:\a\holonomy\holonomy\crates\holonomy-shell` on
    // `windows-latest`, and a splitter that only knows `/` treats that whole string as one
    // segment — so `pop` had nothing to pop and every assertion below failed there while
    // passing here. Comparing in a single separator spelling makes the check a statement
    // about *levels*, which is what it is about, on both platforms. The filesystem check at
    // the end then uses the native path, because that is the one that has to be real.
    let conf = config();
    let tauri = tauri_dir();
    let tauri_slashed = tauri.replace('\\', "/");
    let parent_of_tauri = pop(&tauri_slashed, 1);
    let repo = pop(&tauri_slashed, 2); // crates/holonomy-shell -> the repository root

    for key in ["beforeDevCommand", "beforeBuildCommand"] {
        let prefix = before_command_prefix(&conf, key);
        let resolved = normalise(&parent_of_tauri, &prefix);
        assert_eq!(
            resolved,
            format!("{repo}/app"),
            "build.{key} points npm at `{prefix}`, which from the parent of the tauri \
             directory (`{parent_of_tauri}`) resolves to `{resolved}`. It must resolve to \
             `{repo}/app`, the directory holding package.json, counted from where Tauri runs \
             the command."
        );
    }

    let dist = conf["build"]["frontendDist"]
        .as_str()
        .expect("build.frontendDist must be a string");
    let resolved_dist = normalise(&tauri_slashed, dist);
    assert_eq!(
        resolved_dist,
        format!("{repo}/app/dist"),
        "build.frontendDist is `{dist}`, which from the tauri directory resolves to \
         `{resolved_dist}` rather than `{repo}/app/dist`"
    );

    // `app/package.json` is committed, so this holds on every runner and is a real
    // invariant: the `--prefix` above names the actual frontend package.
    //
    // `app/dist` is deliberately **not** asserted to exist. It is a build product, and
    // whether it is present depends on which step ran first — the `rust` job never builds
    // the frontend, so `cargo test -p holonomy-shell` there fails on a directory that is
    // correctly absent. Asserting it made this test a claim about the runner's step order
    // wearing a claim about the config's spelling, and it cost the whole `rust` leg:
    //
    // ```text
    //   `/home/runner/work/holonomy/holonomy/app/dist` is where Tauri will look for the
    //    built frontend, and it does not exist.
    // ```
    //
    // The spelling is the part that was wrong for months, and it is checked above as text,
    // which is why it needed no build to verify in the first place.
    // The `--prefix` above, resolved against the *native* tauri path so this is a real path
    // on Windows too. `Path::join` keeps the `..` and the OS resolves it, which is why this
    // is `join` rather than the string arithmetic above.
    let native_prefix_dir = std::path::Path::new(&tauri)
        .parent()
        .expect("the tauri directory has a parent")
        .join(before_command_prefix(&conf, "beforeBuildCommand"));
    assert!(
        native_prefix_dir.join("package.json").is_file(),
        "{} is where build.beforeBuildCommand tells npm to look, and there is no \
         package.json there",
        native_prefix_dir.display()
    );
}

#[test]
fn the_extension_in_the_association_is_the_one_the_format_defines() {
    let conf = config();
    let assoc = conf["bundle"]["fileAssociations"]
        .as_array()
        .expect("bundle.fileAssociations must be a list");

    assert!(
        !assoc.is_empty(),
        "no file association is declared, so `.{}` files will not be offered to Holonomy",
        extension()
    );

    let ours: Vec<&Value> = assoc
        .iter()
        .filter(|a| {
            a["ext"]
                .as_array()
                .is_some_and(|exts| exts.iter().any(|e| e.as_str() == Some(extension().as_str())))
        })
        .collect();

    assert_eq!(
        ours.len(),
        1,
        "expected exactly one association for `.{}`, found {}",
        extension(),
        ours.len()
    );
}

#[test]
fn the_association_carries_everything_a_bundler_needs() {
    // Each field is required by a specific bundler, and each omission fails silently on
    // some platform and loudly on another — which is why they are asserted rather than
    // assumed present.
    let conf = config();
    let a = conf["bundle"]["fileAssociations"][0].clone();

    assert_eq!(
        a["mimeType"].as_str(),
        Some(holonomy_core::holo::MIME),
        "the declared MIME type is not the one holonomy_core defines, so the two would \
         disagree about what a .holo file is"
    );

    // `role` is what tells macOS whether Holonomy may edit or only view the file. Omitting it
    // or getting it wrong makes the app open read-only on one platform and not the others.
    assert_eq!(
        a["role"].as_str(),
        Some("Editor"),
        "role must be Editor: anything else makes the app read-only on macOS"
    );

    // `name` is the label the OS shows in "Open with" and in the file-type list. An empty one
    // is not an error on any platform and is wrong on all three.
    assert!(
        a["name"].as_str().is_some_and(|n| !n.trim().is_empty()),
        "the association has no name, so the OS will show an empty entry"
    );

    assert!(
        a["description"].as_str().is_some_and(|d| !d.trim().is_empty()),
        "the association has no description"
    );
}

#[test]
fn the_csp_permits_what_the_frontend_loads_at_runtime() {
    // # The reasoning, and why it is here rather than in a comment
    //
    // The frontend loads three kinds of thing that a policy can refuse:
    //
    //   * the app bundle itself — `'self'`, covered;
    //   * `holo-asset://` images — needs `img-src` to include `holo-asset:`, and it does;
    //   * KaTeX's stylesheet and its webfonts — the stylesheet is bundled so `'self'` covers
    //     it, and the fonts inside it are same-origin relative URLs, so whichever directive
    //     governs `font-src` must allow `'self'`. `font-src` is **absent**, so it inherits
    //     `default-src 'self'`, which allows them.
    //
    // That last step is the fragile one. Adding `font-src` without `'self'`, or tightening
    // `default-src`, breaks every equation in the product while breaking nothing else. This
    // asserts the *inheritance* explicitly, because "the directive is absent" is the property
    // that makes the outcome correct and it is invisible to any runtime test that happens to
    // pass.
    let csp = config()["app"]["security"]["csp"]
        .as_str()
        .expect("a CSP must be configured; without one every engine applies its own defaults")
        .to_string();

    // Trim *before* splitting. The first version split the raw slice, so the leading space
    // after each `;` was the whitespace found and every key came out as the empty string --
    // which then read as "no img-src" and made the whole assertion vacuously true about the
    // fallback. A parser that cannot parse its own input is a parser that reports whatever
    // it wants.
    let directives: Vec<(String, String)> = csp
        .split(';')
        .filter_map(|d| d.trim().split_once(char::is_whitespace))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    assert!(
        directives.iter().any(|(k, _)| k == "default-src"),
        "no directive parsed out of {csp:?} at all, so every assertion below would be vacuous"
    );
    let find = |name: &str| directives.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());

    // 1. The image scheme.
    let img = find("img-src").expect("no img-src: images would fall back to default-src");
    assert!(
        img.contains("holo-asset:"),
        "img-src is {img:?} and does not include holo-asset:, so every stored image is \
         blocked under the production policy"
    );
    // `blob:` is required and `data:` is forbidden, and the two halves are the same decision.
    //
    // The ingest path stores bytes and addresses them by digest; the resolver turns them into
    // an *object* URL. `data:` was allowed here on the stated grounds that "the paste path
    // produces data URLs, before the bytes reach `holo-asset://`". Nothing does that.
    // `ingest.ts` throws if a figure address ever is one — `isInlineDataUrl` exists for
    // exactly that — `assets.ts` carries an `assertNoInlineImages` document walk, and the
    // production bundle contains zero `data:image` strings. The premise was false, and the
    // assertion pinned it.
    //
    // So the allowance is gone. A `data:` image is the shape an injected payload takes, and
    // this application reads untrusted documents; leaving the one scheme that embeds bytes
    // into a document open, in a project whose invariant is that bytes never go into a
    // document, was the wrong side to err on.
    assert!(img.contains("blob:"), "img-src is {img:?}: object URLs would be blocked");
    assert!(
        !img.contains("data:"),
        "img-src is {img:?}, which allows inline `data:` images. The asset pipeline stores bytes \
         and addresses them by digest; a document must never carry an inline payload, and this is \
         the directive that would stop one rendering if it got in."
    );

    // 2. Fonts, by inheritance or by name — and *which* of the two, stated.
    let fonts = find("font-src");
    let default_src = find("default-src").expect("no default-src");
    assert!(
        default_src.contains("'self'"),
        "default-src is {default_src:?}; with no font-src it governs fonts, and without \
         'self' every KaTeX webfont is blocked"
    );
    match &fonts {
        Some(explicit) => assert!(
            explicit.contains("'self'"),
            "font-src is {explicit:?}, which does not allow 'self', so the KaTeX webfonts \
             the stylesheet references are blocked"
        ),
        None => { /* covered by the default-src assertion above */ }
    }

    // 3. Styles. KaTeX's stylesheet is bundled, but the app's own `index.html` also has a
    // `<style>` block, and KaTeX injects rules at runtime.
    let style = find("style-src").expect("no style-src");
    assert!(
        style.contains("'unsafe-inline'"),
        "style-src is {style:?} without 'unsafe-inline'. KaTeX writes style attributes and \
         the app has an inline <style> block, so both would be blocked"
    );

    // 4. Scripts must *not* be permissive. This is the directive whose weakening would be a
    //    security regression rather than a rendering one, and no test above notices it.
    let script = find("script-src").expect("no script-src");
    assert!(
        !script.contains("'unsafe-eval'") && !script.contains("'unsafe-inline'"),
        "script-src is {script:?}. Both are legitimate to want and neither is: this is a \
         word processor reading untrusted documents, and an eval-capable script-src undoes \
         every other line here"
    );
    assert!(
        script.contains("'self'"),
        "script-src is {script:?}: nothing else can load a script, so it must allow 'self'"
    );
}

#[test]
fn the_bundle_targets_and_identity_are_declared() {
    // Not a test of anything clever — a check that the values a bundler needs are *present*,
    // which is the failure mode of a config edited by hand.
    let bundle = &config()["bundle"];
    assert_eq!(
        bundle["active"].as_bool(),
        Some(true),
        "bundling is switched off, so `cargo tauri build` produces a binary and nothing else"
    );
    assert!(bundle["targets"].is_string() || bundle["targets"].is_array());

    let identity = &config()["app"];
    assert!(identity["withGlobalTauri"].as_bool() == Some(true));

    // Each declared icon must exist, and a `.ico` must be among them.
    //
    // Both halves are here because of a platform that is not the one being developed on.
    // `tauri-build` resolves the Windows resource icon from `bundle.icon`, takes the first
    // entry ending in `.ico`, falls back to a literal `icons/icon.ico`, and *returns an
    // error* if that path does not exist — so a list of three PNGs is a green build on Linux
    // and macOS and a hard build failure on Windows, with an error naming a file nobody
    // remembered to add. See `tauri-build-2.7.1/src/lib.rs`, the `target_triple.contains(
    // "windows")` block. A list that is merely non-empty cannot catch it.
    let icons: Vec<&str> = bundle["icon"]
        .as_array()
        .expect("no icon list")
        .iter()
        .map(|v| v.as_str().expect("an icon entry that is not a string"))
        .collect();
    assert!(
        !icons.is_empty(),
        "no icons declared, so a packaged app has no icon and the installer may refuse"
    );
    assert!(
        icons.iter().any(|i| i.ends_with(".ico")),
        "no `.ico` among {icons:?}. Windows resolves the resource icon from this list and \
         fails the build when it finds none, and that failure cannot be seen from Linux."
    );
    for icon in &icons {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/").to_string() + icon;
        assert!(
            std::path::Path::new(&path).exists(),
            "bundle.icon declares {icon:?}, which is not in the tree. It builds on the \
             platform where the icon is not needed and fails on the one where it is."
        );
    }
}

#[test]
fn the_product_identity_is_the_one_the_package_is_registered_under() {
    // `identifier` becomes the reverse-DNS bundle id, the Windows registry key and the macOS
    // bundle id. Changing it after release orphans every existing installation, so it is
    // pinned here rather than trusted to review.
    assert_eq!(
        config()["identifier"].as_str(),
        Some("dev.holonomy.app"),
        "the bundle identifier changed. This orphans every installed copy and invalidates the \
         document file association already registered on users' machines."
    );
}