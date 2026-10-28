//! Loading JSON from a local path or a URL (blocking), or the default, with a warning, if that fails (e.g. offline).

use reqwest::Url;
use serde::de::DeserializeOwned;
use std::fs;
use std::path::Path;

pub trait ReadableFromPath {
    fn from_path(path: &str, typename: &str) -> Self
    where
        Self: Sized + DeserializeOwned + Default,
    {
        println!("Loading {} from {}", typename, path);
        let loaded = read(path).and_then(|content| serde_json::from_str(&content).map_err(|e| format!("it isn't one: {}", e)));
        loaded.unwrap_or_else(|why| {
            eprintln!("Warning: couldn't load the {} from {} ({}), so there's none", typename, path, why);
            Self::default()
        })
    }
}

// What's in the file at `path`, or at it if it's a web address
fn read(path: &str) -> Result<String, String> {
    if Path::new(path).exists() {
        return fs::read_to_string(path).map_err(|e| e.to_string());
    }
    match Url::parse(path) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => reqwest::blocking::get(url)
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.text())
            .map_err(|e| e.to_string()),
        _ => Err("there's no such file".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Default, Deserialize, PartialEq, Debug)]
    struct Values {
        values: Vec<u32>,
    }

    impl ReadableFromPath for Values {}

    #[test]
    fn what_cant_be_loaded_is_the_default() {
        let dir = std::env::temp_dir().join(format!("tomb-helper-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (good, bad) = (dir.join("good.json"), dir.join("bad.json"));
        fs::write(&good, r#"{"values": [1, 2]}"#).unwrap();
        fs::write(&bad, "<html>").unwrap();
        assert_eq!(Values::from_path(good.to_str().unwrap(), "values"), Values { values: vec![1, 2] });
        assert_eq!(Values::from_path(bad.to_str().unwrap(), "values"), Values::default());
        assert_eq!(Values::from_path("no such file.json", "values"), Values::default());
        // Nothing listens on the discard port, as when offline nothing answers
        assert_eq!(Values::from_path("http://127.0.0.1:9/values.json", "values"), Values::default());
        fs::remove_dir_all(&dir).unwrap();
    }
}
