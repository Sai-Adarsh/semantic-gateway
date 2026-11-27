use crate::{Result, cache};
use reqwest::{Client, Url};
use serde::Deserialize;
use std::time::Duration;

pub struct Api {
    client: Client,
    embeddings: Endpoint,
    generation: Endpoint,
}

struct Endpoint {
    url: Url,
    model: String,
    key: Option<String>,
}

impl Endpoint {
    fn from_env(prefix: &str) -> Result<Self> {
        let url = Url::parse(&std::env::var(format!("{prefix}_URL"))?)?;
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(format!("{prefix}_URL must be HTTP(S) without URL credentials").into());
        }
        let model = std::env::var(format!("{prefix}_MODEL"))?;
        if model.trim().is_empty() {
            return Err(format!("{prefix}_MODEL cannot be empty").into());
        }
        Ok(Self { url, model, key: std::env::var(format!("{prefix}_API_KEY")).ok() })
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self, client: &Client, payload: serde_json::Value,
    ) -> Result<T> {
        let mut request = client.post(self.url.clone()).json(&payload);
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        Ok(request.send().await?.error_for_status()?.json().await?)
    }
}

#[derive(Deserialize)]
struct Embeddings {
    data: Vec<Embedding>,
}

#[derive(Deserialize)]
struct Embedding {
    embedding: Vec<f64>,
}

#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    content: String,
}

pub enum Backend {
    Demo,
    Http(Box<Api>),
}

impl Backend {
    pub fn from_env() -> Result<Self> {
        Ok(Self::Http(Box::new(Api {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            embeddings: Endpoint::from_env("EMBEDDING")?,
            generation: Endpoint::from_env("GENERATION")?,
        })))
    }

    pub async fn embed(&self, prompt: &str) -> Result<Vec<f64>> {
        let vector = match self {
            Self::Demo => fixture_embedding(prompt),
            Self::Http(api) => {
                let response: Embeddings = api.embeddings.send(&api.client, serde_json::json!({
                    "model": api.embeddings.model,
                    "input": prompt,
                    "encoding_format": "float"
                })).await?;
                response.data.into_iter().next().ok_or("missing embedding")?.embedding
            }
        };
        cache::normalize(vector).ok_or_else(|| "upstream returned an invalid embedding".into())
    }

    pub async fn generate(&self, prompt: &str) -> Result<String> {
        let answer = match self {
            Self::Demo => format!("Fixture response for: {prompt}"),
            Self::Http(api) => {
                let response: Completion = api.generation.send(&api.client, serde_json::json!({
                    "model": api.generation.model,
                    "messages": [{ "role": "user", "content": prompt }],
                    "max_tokens": 256,
                    "temperature": 0,
                    "stream": false
                })).await?;
                response.choices.into_iter().next().ok_or("missing completion")?.message.content
            }
        };
        if answer.len() > 64 * 1024 {
            return Err("completion exceeds the gateway response limit".into());
        }
        Ok(answer)
    }
}

fn fixture_embedding(prompt: &str) -> Vec<f64> {
    fn bucket(word: &str) -> usize {
        let hash = word.bytes().fold(2_166_136_261_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
        });
        (hash % 256) as usize
    }

    let lower = prompt.to_lowercase();
    let mut vector = vec![0.0; 256];
    let mut words = 0;
    for word in lower.split(|ch: char| !ch.is_alphanumeric()).filter(|word| !word.is_empty()) {
        let normalized = match word {
            "a" | "also" | "the" | "give" | "me" | "of" | "please" => continue,
            "summary" | "summarise" | "summarize" => "summary",
            other => other,
        };
        vector[bucket(normalized)] += 1.0;
        words += 1;
    }
    if words == 0 {
        vector[bucket(&lower)] = 1.0;
    }
    vector
}
