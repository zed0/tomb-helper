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
        let file_path = Path::new(path);
        let url = Url::parse(path);

        let content = if file_path.exists() {
            fs::read_to_string(file_path).unwrap()
        }
        else if url.is_ok() && !url.as_ref().unwrap().cannot_be_a_base() {
            reqwest::blocking::get(url.unwrap().as_str())
                .expect(format!("Could not retrieve {} url", typename).as_str())
                .text()
                .unwrap()
        }
        else {
            println!("Could not read {}, using default!", typename);
            return Default::default();
        };

        let result = serde_json::from_str(&content)
            .expect(format!("Could not parse {} to expected format", typename).as_str());
        return result;
    }
}
