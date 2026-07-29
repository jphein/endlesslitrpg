use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use litrpg_config::{
    CONFIG_ENV, Config, ConfigError, DEFAULT_CONFIG_RELPATH, MIN_BUFFER_TARGET, STARTER_CONFIG,
    expand_tilde, resolve_config_path,
};
use tempfile::TempDir;

/// Randomly-named temp dir, removed on drop. Random naming matters: a predictable
/// path left behind by a crashed run would make `write_default_if_absent` see an
/// existing file and fail a later run for reasons having nothing to do with the code.
fn tmp() -> TempDir {
    tempfile::Builder::new()
        .prefix("litrpg-config-")
        .tempdir()
        .unwrap()
}

fn write(path: &Path, body: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn home() -> PathBuf {
    dirs::home_dir().expect("tests need a home directory")
}

// ---------------------------------------------------------------- defaults

#[test]
fn defaults_match_the_spec() {
    let c = Config::default();
    assert_eq!(c.ember_url, "http://familiar:8091");
    assert_eq!(c.ember_model, "qwen36-coder");
    assert_eq!(c.bind_addr, "0.0.0.0:8093");
    assert_eq!(c.buffer_target, 3);
    assert_eq!(c.target_words, 2000);
    assert_eq!(c.narrator_voice, "sherpa:piper-en_GB-cori-high:0");
}

#[test]
fn defaults_are_valid() {
    // Whatever the defaults are, they must survive our own validation gate —
    // otherwise `load()` on a machine with no config file cannot succeed.
    let mut c = Config::default();
    c.db_path = expand_tilde(&c.db_path);
    c.media_dir = expand_tilde(&c.media_dir);
    c.story_dir = expand_tilde(&c.story_dir);
    c.validate().unwrap();
}

#[test]
fn default_bind_addr_parses_to_a_socket_addr() {
    let addr = Config::default().parsed_bind_addr().unwrap();
    assert_eq!(addr.port(), 8093);
    assert!(addr.ip().is_unspecified());
}

// -------------------------------------------------------- file loading

#[test]
fn missing_file_yields_defaults_not_an_error() {
    let dir = tmp();
    let c = Config::load_from(&dir.path().join("nope.toml")).unwrap();
    assert_eq!(c.buffer_target, 3);
    assert_eq!(c.ember_model, "qwen36-coder");
}

#[test]
fn a_full_file_is_read_back_verbatim() {
    let dir = tmp();
    let path = dir.path().join("config.toml");
    write(
        &path,
        r#"
db_path = "/srv/litrpg/story.db"
media_dir = "/srv/litrpg/media"
story_dir = "/srv/litrpg/story"
ember_url = "http://10.0.6.107:9000"
ember_model = "some-other-model"
bind_addr = "127.0.0.1:9999"
buffer_target = 5
target_words = 3500
narrator_voice = "azure:en-GB-RyanNeural:0"
"#,
    );
    let c = Config::load_from(&path).unwrap();
    assert_eq!(c.db_path, PathBuf::from("/srv/litrpg/story.db"));
    assert_eq!(c.ember_url, "http://10.0.6.107:9000");
    assert_eq!(c.bind_addr, "127.0.0.1:9999");
    assert_eq!(c.buffer_target, 5);
    assert_eq!(c.target_words, 3500);
    assert_eq!(c.narrator_voice, "azure:en-GB-RyanNeural:0");
}

#[test]
fn a_partial_file_falls_back_per_missing_key() {
    let dir = tmp();
    let path = dir.path().join("config.toml");
    // Only two keys set. Everything else must fall back, not fail.
    write(
        &path,
        r#"
buffer_target = 7
ember_model = "custom"
"#,
    );
    let c = Config::load_from(&path).unwrap();
    assert_eq!(c.buffer_target, 7);
    assert_eq!(c.ember_model, "custom");
    // Untouched keys keep their defaults.
    assert_eq!(c.ember_url, "http://familiar:8091");
    assert_eq!(c.bind_addr, "0.0.0.0:8093");
    assert_eq!(c.target_words, 2000);
    assert_eq!(c.narrator_voice, "sherpa:piper-en_GB-cori-high:0");
}

#[test]
fn an_empty_file_is_all_defaults() {
    let dir = tmp();
    let path = dir.path().join("config.toml");
    write(&path, "");
    assert_eq!(Config::load_from(&path).unwrap().buffer_target, 3);
}

#[test]
fn malformed_toml_is_an_error_not_a_silent_fallback() {
    let dir = tmp();
    let path = dir.path().join("config.toml");
    write(&path, "this is not = = toml");
    let err = Config::load_from(&path).unwrap_err();
    assert!(
        matches!(err, ConfigError::Parse { .. }),
        "expected Parse, got {err:?}"
    );
}

#[test]
fn an_unknown_key_is_rejected_rather_than_ignored() {
    // A typo'd key silently doing nothing is how you spend an hour wondering why
    // buffer_target had no effect. `deny_unknown_fields` turns it into an error.
    let dir = tmp();
    let path = dir.path().join("config.toml");
    write(&path, "bufer_target = 9\n");
    assert!(matches!(
        Config::load_from(&path).unwrap_err(),
        ConfigError::Parse { .. }
    ));
}

// ------------------------------------------------------ tilde expansion

#[test]
fn tilde_is_expanded_in_all_three_paths() {
    let dir = tmp();
    let path = dir.path().join("config.toml");
    write(
        &path,
        r#"
db_path = "~/litrpg/story.db"
media_dir = "~/litrpg/media"
story_dir = "~/litrpg/story"
"#,
    );
    let c = Config::load_from(&path).unwrap();
    assert_eq!(c.db_path, home().join("litrpg/story.db"));
    assert_eq!(c.media_dir, home().join("litrpg/media"));
    assert_eq!(c.story_dir, home().join("litrpg/story"));
    assert!(!c.db_path.to_str().unwrap().contains('~'));
}

#[test]
fn a_bare_tilde_expands_to_home() {
    assert_eq!(expand_tilde(Path::new("~")), home());
}

#[test]
fn a_tilde_that_is_not_a_home_prefix_is_left_alone() {
    // These are legal filenames. Rewriting them would corrupt paths.
    assert_eq!(
        expand_tilde(Path::new("/srv/back~up/story.db")),
        PathBuf::from("/srv/back~up/story.db")
    );
    assert_eq!(
        expand_tilde(Path::new("~user/story.db")),
        PathBuf::from("~user/story.db")
    );
    assert_eq!(
        expand_tilde(Path::new("./~/story.db")),
        PathBuf::from("./~/story.db")
    );
}

#[test]
fn absolute_paths_survive_expansion_untouched() {
    assert_eq!(
        expand_tilde(Path::new("/srv/litrpg/story.db")),
        PathBuf::from("/srv/litrpg/story.db")
    );
}

#[test]
fn prompt_path_is_prompt_md_under_story_dir() {
    let c = Config {
        story_dir: PathBuf::from("/srv/litrpg/story"),
        ..Default::default()
    };
    assert_eq!(
        c.prompt_path(),
        PathBuf::from("/srv/litrpg/story/prompt.md")
    );
}

// --------------------------------------------------- env var resolution

#[test]
fn env_var_takes_precedence_over_the_user_config_dir() {
    let got = resolve_config_path(
        Some(OsStr::new("/etc/litrpg/other.toml")),
        Some(PathBuf::from("/home/jp/.config")),
    );
    assert_eq!(got, Some(PathBuf::from("/etc/litrpg/other.toml")));
}

#[test]
fn without_the_env_var_the_user_config_dir_is_used() {
    let got = resolve_config_path(None, Some(PathBuf::from("/home/jp/.config")));
    assert_eq!(
        got,
        Some(PathBuf::from("/home/jp/.config").join(DEFAULT_CONFIG_RELPATH))
    );
}

#[test]
fn an_empty_env_var_is_treated_as_unset() {
    // `LITRPG_CONFIG= litrpg status` must mean "use the default", not "load the
    // file named empty string".
    let got = resolve_config_path(
        Some(OsStr::new("")),
        Some(PathBuf::from("/home/jp/.config")),
    );
    assert_eq!(
        got,
        Some(PathBuf::from("/home/jp/.config").join(DEFAULT_CONFIG_RELPATH))
    );
}

#[test]
fn env_var_is_tilde_expanded_too() {
    let got = resolve_config_path(Some(OsStr::new("~/mine.toml")), None);
    assert_eq!(got, Some(home().join("mine.toml")));
}

#[test]
fn with_no_env_var_and_no_config_dir_there_is_no_path() {
    assert_eq!(resolve_config_path(None, None), None);
}

#[test]
fn config_path_reads_the_documented_env_var() {
    // Guards the wiring between `config_path` and `resolve_config_path`: the
    // constant must be the name actually consulted.
    assert_eq!(CONFIG_ENV, "LITRPG_CONFIG");
    let via_helper =
        resolve_config_path(std::env::var_os(CONFIG_ENV).as_deref(), dirs::config_dir());
    assert_eq!(litrpg_config::config_path(), via_helper);
}

// ------------------------------------------------------------ validation

fn cfg() -> Config {
    Config {
        db_path: PathBuf::from("/tmp/story.db"),
        media_dir: PathBuf::from("/tmp/media"),
        story_dir: PathBuf::from("/tmp/story"),
        ..Default::default()
    }
}

#[test]
fn buffer_target_below_the_minimum_is_rejected() {
    for bad in [0u32, 1] {
        let mut c = cfg();
        c.buffer_target = bad;
        let err = c.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::BufferTargetTooLow { got, min } if got == bad && min == MIN_BUFFER_TARGET),
            "buffer_target {bad} should be rejected, got {err:?}"
        );
    }
}

#[test]
fn the_minimum_buffer_target_itself_is_accepted() {
    let mut c = cfg();
    c.buffer_target = MIN_BUFFER_TARGET;
    c.validate().unwrap();
}

#[test]
fn a_low_buffer_target_from_a_file_fails_the_load() {
    // Validation must run on the load path, not only when called directly.
    let dir = tmp();
    let path = dir.path().join("config.toml");
    write(&path, "buffer_target = 1\n");
    assert!(matches!(
        Config::load_from(&path).unwrap_err(),
        ConfigError::BufferTargetTooLow { got: 1, .. }
    ));
}

#[test]
fn each_empty_path_is_rejected_and_named() {
    for (field, mutate) in [("db_path", 0usize), ("media_dir", 1), ("story_dir", 2)] {
        let mut c = cfg();
        match mutate {
            0 => c.db_path = PathBuf::new(),
            1 => c.media_dir = PathBuf::new(),
            _ => c.story_dir = PathBuf::new(),
        }
        let err = c.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::EmptyPath { field: f } if f == field),
            "expected EmptyPath({field}), got {err:?}"
        );
    }
}

#[test]
fn an_unparseable_bind_addr_is_rejected() {
    for bad in ["not-an-addr", "0.0.0.0", "0.0.0.0:notaport", ""] {
        let mut c = cfg();
        c.bind_addr = bad.into();
        let err = c.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::BadBindAddr { .. }),
            "bind_addr {bad:?} should be rejected, got {err:?}"
        );
    }
}

#[test]
fn a_bind_addr_without_a_port_is_rejected() {
    // Easy mistake, and the failure would otherwise surface as a confusing
    // bind error deep inside the daemon at startup.
    let mut c = cfg();
    c.bind_addr = "10.0.6.107".into();
    assert!(matches!(
        c.validate().unwrap_err(),
        ConfigError::BadBindAddr { .. }
    ));
}

#[test]
fn an_ipv6_bind_addr_is_accepted() {
    let mut c = cfg();
    c.bind_addr = "[::1]:8093".into();
    c.validate().unwrap();
    assert_eq!(c.parsed_bind_addr().unwrap().port(), 8093);
}

#[test]
fn zero_target_words_is_rejected() {
    let mut c = cfg();
    c.target_words = 0;
    assert!(matches!(
        c.validate().unwrap_err(),
        ConfigError::ZeroTargetWords
    ));
}

#[test]
fn blank_strings_are_rejected_per_field() {
    let mut c = cfg();
    c.ember_url = "   ".into();
    assert!(matches!(
        c.validate().unwrap_err(),
        ConfigError::EmptyEmberUrl
    ));

    let mut c = cfg();
    c.ember_model = "".into();
    assert!(matches!(
        c.validate().unwrap_err(),
        ConfigError::EmptyEmberModel
    ));

    let mut c = cfg();
    c.narrator_voice = "\t".into();
    assert!(matches!(
        c.validate().unwrap_err(),
        ConfigError::EmptyNarratorVoice
    ));
}

// ------------------------------------------------ write_default_if_absent

#[test]
fn writes_a_starter_file_when_absent() {
    let dir = tmp();
    let path = dir.path().join("config.toml");
    assert!(Config::write_default_if_absent(&path).unwrap());
    assert!(path.exists());
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("db_path"));
    assert!(body.starts_with('#'), "starter file should be commented");
}

#[test]
fn never_overwrites_an_existing_file() {
    let dir = tmp();
    let path = dir.path().join("config.toml");
    write(&path, "buffer_target = 4\n");
    assert!(!Config::write_default_if_absent(&path).unwrap());
    // The user's content is intact.
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "buffer_target = 4\n"
    );
    assert_eq!(Config::load_from(&path).unwrap().buffer_target, 4);
}

#[test]
fn creates_missing_parent_directories() {
    let dir = tmp();
    let path = dir.path().join("a/b/c/config.toml");
    assert!(Config::write_default_if_absent(&path).unwrap());
    assert!(path.exists());
}

#[test]
fn the_starter_file_loads_and_equals_the_defaults() {
    // The commented starter file documents the defaults. If someone edits one
    // and not the other, the file would start lying — this catches that.
    let dir = tmp();
    let path = dir.path().join("config.toml");
    Config::write_default_if_absent(&path).unwrap();

    let loaded = Config::load_from(&path).unwrap();
    let mut expected = Config::default();
    expected.db_path = expand_tilde(&expected.db_path);
    expected.media_dir = expand_tilde(&expected.media_dir);
    expected.story_dir = expand_tilde(&expected.story_dir);

    assert_eq!(loaded, expected);
}

#[test]
fn the_starter_constant_parses_on_its_own() {
    let parsed: Config = toml::from_str(STARTER_CONFIG).unwrap();
    assert_eq!(parsed.buffer_target, 3);
}
