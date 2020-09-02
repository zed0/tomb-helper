use reqwest::Url;
use std::path::Path;
use std::fs;
use std::default::Default;
use serde::de::DeserializeOwned;

pub trait ReadableFromPath {
    fn from_path(
        path: &String,
        typename: &String
    ) -> Self
        where
            Self: std::marker::Sized + DeserializeOwned + Default
    {
        println!("Loading {} from {}", typename, path);
        let url = Url::parse(path).unwrap();
        let content;

        if url.has_host() {
            content = reqwest::blocking::get(url.as_str())
                .expect(format!("Could not retrieve {} url", typename).as_str())
                .text()
                .unwrap();
        }
        else {
            let file_path = Path::new(path);
            if file_path.exists() {
                content = fs::read_to_string(file_path).unwrap();
            }
            else {
                return Default::default();
            }
        }

        let result = serde_json::from_str(&content)
            .expect(format!("Could not parse {} to expected format", typename).as_str());
        return result;
    }
}
