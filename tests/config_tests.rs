use fmcd::config::Config;
use tempfile::tempdir;

#[test]
fn test_default_config() {
    let config = Config::default();
    assert_eq!(config.http_bind_ip, "127.0.0.1");
    assert_eq!(config.http_bind_port, 7070);
    assert!(config.http_password.is_none());
    assert!(!config.is_auth_enabled());
}

#[test]
fn test_config_with_auth() {
    let config = Config {
        http_password: Some("testpassword".to_string()),
        ..Default::default()
    };
    assert!(config.is_auth_enabled());
    assert_eq!(config.auth_password(), Some("testpassword"));
}

#[test]
fn test_config_addresses() {
    let config = Config::default();
    assert_eq!(config.http_address(), "127.0.0.1:7070");
    assert_eq!(config.websocket_address(), "127.0.0.1:7070");

    let config_with_ws_port = Config {
        websocket_port: Some(9741),
        ..Default::default()
    };
    assert_eq!(config_with_ws_port.websocket_address(), "127.0.0.1:9741");
}

#[test]
fn test_config_save_load() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("test.toml");

    let original_config = Config {
        http_password: Some("testpass".to_string()),
        http_bind_port: 8080,
        ..Default::default()
    };

    // Save config
    original_config.save_to_file(&config_path).unwrap();

    // Load config
    let loaded_config = Config::load_from_file(&config_path).unwrap();

    assert_eq!(loaded_config.http_password, Some("testpass".to_string()));
    assert_eq!(loaded_config.http_bind_port, 8080);
}

#[test]
fn test_generate_password() {
    let password1 = Config::generate_password();
    let password2 = Config::generate_password();

    // Passwords should be different
    assert_ne!(password1, password2);

    // Should be 64 hex characters (32 bytes * 2 hex chars per byte)
    assert_eq!(password1.len(), 64);
    assert_eq!(password2.len(), 64);

    // Should be valid hex
    assert!(password1.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(password2.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_load_or_create_new_file() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("new_config.toml");

    // File doesn't exist
    assert!(!config_path.exists());

    // Load or create should create file and generate password
    let (config, password_generated) = Config::load_or_create(&config_path).unwrap();

    assert!(config_path.exists());
    assert!(password_generated);
    assert!(config.http_password.is_some());

    let password = config.http_password.unwrap();
    assert_eq!(password.len(), 64);
    assert!(password.chars().all(|c| c.is_ascii_hexdigit()));

    // Verify the file contains the password
    let file_contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(file_contents.contains(&format!("http-password = \"{}\"", password)));
}

#[test]
fn test_load_or_create_existing_file_with_password() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("existing_config.toml");

    // Create a config with password
    let original_config = Config {
        http_password: Some("existingpass".to_string()),
        ..Default::default()
    };
    original_config.save_to_file(&config_path).unwrap();

    // Load or create should not generate new password
    let (config, password_generated) = Config::load_or_create(&config_path).unwrap();

    assert!(!password_generated);
    assert_eq!(config.http_password, Some("existingpass".to_string()));
}

#[test]
fn test_load_or_create_existing_file_without_password() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config_no_pass.toml");

    // Create a config without password
    let original_config = Config::default();
    original_config.save_to_file(&config_path).unwrap();

    // Load or create should generate password
    let (config, password_generated) = Config::load_or_create(&config_path).unwrap();

    assert!(password_generated);
    assert!(config.http_password.is_some());

    let password = config.http_password.unwrap();
    assert_eq!(password.len(), 64);

    // Verify the file was updated with the password
    let file_contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(file_contents.contains(&format!("http-password = \"{}\"", password)));

    // Verify no temp files are left behind
    let temp_path = config_path.with_extension("tmp");
    assert!(!temp_path.exists(), "Temporary file should be cleaned up");
}

#[test]
fn test_atomic_save_no_temp_files() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("atomic_test.toml");

    let config = Config {
        http_password: Some("test123".to_string()),
        ..Default::default()
    };

    // Save config atomically
    config.save_to_file(&config_path).unwrap();

    // Verify config was saved
    assert!(config_path.exists());

    // Verify no temp files are left behind
    let temp_path = config_path.with_extension("tmp");
    assert!(
        !temp_path.exists(),
        "Temporary file should be cleaned up after atomic save"
    );

    // Verify content is correct
    let loaded = Config::load_from_file(&config_path).unwrap();
    assert_eq!(loaded.http_password, Some("test123".to_string()));
}
