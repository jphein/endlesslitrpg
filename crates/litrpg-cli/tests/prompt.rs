use litrpg_cli::{CliError, prompt};
use tempfile::TempDir;

/// Randomly-named temp dir, removed on drop. Random naming matters: a predictable
/// path left behind by a crashed run would make `ensure_prompt_file` see an
/// existing file and fail a later run for reasons having nothing to do with the code.
fn tmp() -> TempDir {
    tempfile::Builder::new()
        .prefix("litrpg-prompt-")
        .tempdir()
        .unwrap()
}

/// A fake editor: runs `sh -c <script> sh <path>`, so `$1` is the file.
fn editor_script(script: &str) -> Vec<Vec<String>> {
    vec![vec![
        "sh".to_string(),
        "-c".to_string(),
        script.to_string(),
        "sh".to_string(),
    ]]
}

fn noop_editor() -> Vec<Vec<String>> {
    vec![vec!["true".to_string()]]
}

// ---------------------------------------------------------------- hashing

#[test]
fn content_hash_is_stable_and_algorithm_tagged() {
    let a = prompt::content_hash("the vale smelled of iron");
    assert_eq!(a, prompt::content_hash("the vale smelled of iron"));
    assert!(a.starts_with("fnv1a64:"), "{a}");
    // 8 bytes rendered as hex, plus the tag and colon.
    assert_eq!(a.len(), "fnv1a64:".len() + 16);
}

#[test]
fn content_hash_changes_when_the_text_changes() {
    assert_ne!(prompt::content_hash("a"), prompt::content_hash("b"));
    // Including on a change that many weak hashes miss: transposition.
    assert_ne!(prompt::content_hash("ab"), prompt::content_hash("ba"));
    // And on whitespace, which matters for a prompt.
    assert_ne!(prompt::content_hash("a b"), prompt::content_hash("a  b"));
}

#[test]
fn the_empty_string_hashes_to_the_fnv_offset_basis() {
    // Pins the algorithm: if someone swaps the constants, this fails loudly
    // rather than silently invalidating every stored prompt_hash.
    assert_eq!(prompt::content_hash(""), "fnv1a64:cbf29ce484222325");
}

// ------------------------------------------------------------ file creation

#[test]
fn ensure_creates_the_file_from_the_starter_template() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    assert!(prompt::ensure_prompt_file(&path).unwrap());
    let body = std::fs::read_to_string(&path).unwrap();
    assert_eq!(body, prompt::STARTER_PROMPT);
    assert!(body.contains("Story prompt"));
}

#[test]
fn ensure_creates_missing_parent_directories() {
    let dir = tmp();
    let path = dir.path().join("story/nested/prompt.md");
    assert!(prompt::ensure_prompt_file(&path).unwrap());
    assert!(path.exists());
}

#[test]
fn ensure_never_overwrites_an_existing_prompt() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "my own prompt").unwrap();
    assert!(!prompt::ensure_prompt_file(&path).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "my own prompt");
}

#[test]
fn the_starter_template_is_not_blank_so_it_passes_validation() {
    // Otherwise `litrpg prompt` on a fresh install would create a file and then
    // immediately reject it.
    assert!(!prompt::STARTER_PROMPT.trim().is_empty());
}

// -------------------------------------------------------------- editing

#[test]
fn editing_a_new_file_reports_creation_and_a_hash() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    let o = prompt::edit_prompt(&path, &noop_editor()).unwrap();
    assert!(o.created);
    assert_eq!(o.hash, prompt::content_hash(prompt::STARTER_PROMPT));
    assert_eq!(o.bytes, prompt::STARTER_PROMPT.len());
}

#[test]
fn a_freshly_created_template_says_it_is_unedited_not_unchanged() {
    // "No change" is true but useless here: the story has no premise yet.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    let o = prompt::edit_prompt(&path, &noop_editor()).unwrap();
    assert!(o.created && !o.changed);
    let out = prompt::render_text(&o);
    assert!(out.contains("unedited starter template"), "{out}");
    assert!(!out.contains("No change"), "{out}");
}

#[test]
fn a_template_left_by_init_is_still_reported_as_unedited() {
    // The init -> prompt handoff: `litrpg init` creates the template, so this call
    // has created == false. Keying the message on `created` would report "no change"
    // while the story still has no premise. Found by smoke-testing the real binary.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, prompt::STARTER_PROMPT).unwrap();

    let o = prompt::edit_prompt(&path, &noop_editor()).unwrap();
    assert!(!o.created, "the file already existed");
    assert!(!o.changed);
    assert!(o.is_placeholder);
    let out = prompt::render_text(&o);
    assert!(out.contains("unedited starter template"), "{out}");
    assert!(!out.contains("No change"), "{out}");
}

#[test]
fn an_edited_prompt_is_not_a_placeholder() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "# A real premise\n").unwrap();
    let o = prompt::edit_prompt(&path, &noop_editor()).unwrap();
    assert!(!o.is_placeholder);
}

#[test]
fn an_editor_that_changes_nothing_reports_no_change() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "a stable prompt\n").unwrap();

    let o = prompt::edit_prompt(&path, &noop_editor()).unwrap();
    assert!(!o.created);
    assert!(!o.changed);
    assert_eq!(o.hash, o.previous_hash);
    assert!(prompt::render_text(&o).contains("No change"));
}

#[test]
fn an_edit_is_detected_and_the_new_hash_reported() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "old premise\n").unwrap();
    let before = prompt::content_hash("old premise\n");

    let o = prompt::edit_prompt(&path, &editor_script("printf 'new premise\\n' > \"$1\"")).unwrap();
    assert!(o.changed);
    assert_eq!(o.previous_hash, before);
    assert_eq!(o.hash, prompt::content_hash("new premise\n"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new premise\n");
}

#[test]
fn the_change_message_says_when_it_takes_effect() {
    // §9.3: reload happens at chapter boundaries only. If the CLI does not say so,
    // the natural assumption is that the edit applies to whatever is rendering now.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "old\n").unwrap();
    let o = prompt::edit_prompt(&path, &editor_script("printf 'new\\n' > \"$1\"")).unwrap();
    let out = prompt::render_text(&o);
    assert!(out.contains("next chapter boundary"), "{out}");
}

#[test]
fn emptying_the_prompt_is_an_error() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "something\n").unwrap();

    let err = prompt::edit_prompt(&path, &editor_script(": > \"$1\"")).unwrap_err();
    assert!(
        matches!(&err, CliError::EmptyPrompt { path: p } if p == &path),
        "got {err:?}"
    );
}

#[test]
fn a_whitespace_only_prompt_is_also_rejected() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "something\n").unwrap();
    let err =
        prompt::edit_prompt(&path, &editor_script("printf '  \\n\\t\\n' > \"$1\"")).unwrap_err();
    assert!(matches!(err, CliError::EmptyPrompt { .. }), "got {err:?}");
}

#[test]
fn an_editor_that_exits_nonzero_is_reported_not_ignored() {
    // "nano exited 1" and "nano is not installed" need different responses, so a
    // failing editor must not fall through to the next candidate.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    let candidates = vec![vec!["false".to_string()], vec!["true".to_string()]];
    let err = prompt::edit_prompt(&path, &candidates).unwrap_err();
    assert!(matches!(err, CliError::EditorFailed { .. }), "got {err:?}");
}

#[test]
fn a_missing_editor_falls_through_to_the_next_candidate() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    let candidates = vec![
        vec!["litrpg-no-such-editor-xyz".to_string()],
        vec!["true".to_string()],
    ];
    let o = prompt::edit_prompt(&path, &candidates).unwrap();
    assert!(o.created);
}

#[test]
fn exhausting_every_candidate_reports_what_was_tried() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    let candidates = vec![
        vec!["litrpg-no-such-editor-a".to_string()],
        vec!["litrpg-no-such-editor-b".to_string()],
    ];
    let err = prompt::edit_prompt(&path, &candidates).unwrap_err();
    match err {
        CliError::NoEditor { tried } => {
            assert!(tried.contains("litrpg-no-such-editor-a"), "{tried}");
            assert!(tried.contains("litrpg-no-such-editor-b"), "{tried}");
        }
        other => panic!("expected NoEditor, got {other:?}"),
    }
}

#[test]
fn the_file_still_exists_after_a_failed_edit() {
    // ensure_prompt_file runs first; a later failure must not delete the file.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    let _ = prompt::edit_prompt(&path, &[vec!["false".to_string()]]);
    assert!(path.exists());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        prompt::STARTER_PROMPT
    );
}

// ------------------------------------------------------- editor resolution

#[test]
fn editor_resolution_prefers_env_then_nano_then_vi() {
    let c = prompt::resolve_editor(Some("nvim"));
    assert_eq!(c[0], vec!["nvim".to_string()]);
    assert_eq!(c[1], vec!["nano".to_string()]);
    assert_eq!(c[2], vec!["vi".to_string()]);
}

#[test]
fn without_editor_set_nano_comes_first() {
    // JP's editor is nano (CLAUDE.md), so it is the first fallback, not vi.
    let c = prompt::resolve_editor(None);
    assert_eq!(c[0], vec!["nano".to_string()]);
    assert_eq!(c[1], vec!["vi".to_string()]);
    assert_eq!(c.len(), 2);
}

#[test]
fn a_multi_word_editor_is_split_into_argv() {
    let c = prompt::resolve_editor(Some("code --wait"));
    assert_eq!(c[0], vec!["code".to_string(), "--wait".to_string()]);
}

#[test]
fn a_blank_editor_var_is_ignored_rather_than_spawning_nothing() {
    for blank in ["", "   ", "\t"] {
        let c = prompt::resolve_editor(Some(blank));
        assert_eq!(c[0], vec!["nano".to_string()], "EDITOR={blank:?}");
    }
}

#[test]
fn the_path_is_passed_to_the_editor_as_the_final_argument() {
    // Guards the argv order: `$EDITOR <args...> <path>`.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    // Writes the last argument's own name into the file, proving the path landed
    // in the final position after the fixed args.
    let o = prompt::edit_prompt(
        &path,
        &[vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf 'got:%s\\n' \"$1\" > \"$1\"".to_string(),
            "sh".to_string(),
        ]],
    )
    .unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert_eq!(body, format!("got:{}\n", path.display()));
    assert!(o.changed);
}
