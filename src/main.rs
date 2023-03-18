use std::{error::Error, env};
use async_openai::{types::{CreateChatCompletionRequestArgs, ChatCompletionRequestMessageArgs, Role}, Client};
use tokio::{fs::*, io::{BufReader, AsyncBufReadExt}};
use rand::prelude::*;
use regex::Regex;
use censor::Censor;
use serde_json;

#[derive(Debug)]
enum PromptType {
    Text,
    Image,
}
impl From<&str> for PromptType {
    fn from(value: &str) -> Self {
        match value {
            "text" => PromptType::Text,
            "image" => PromptType::Image,
            _ => PromptType::Text,
        }
    }
}
#[derive(Debug)]
struct Prompt {
    prompt_type: PromptType,
    prompt: String,
}

impl Prompt {
    async fn load_fill_in_file(&self, file_name: String) -> Result<Vec<String>, Box<dyn Error>> {
        let mut fill_ins: Vec<String> = vec![];
        let fill_in_path = format!("resources/{}.txt", file_name);
        let file = File::open(fill_in_path).await?;
        let buf_reader = BufReader::new(file);
        let mut lines = buf_reader.lines();
        while let Some(line) = lines.next_line().await? {
            if !line.starts_with("#") {
                fill_ins.push(line);
            }
        }
        Ok(fill_ins)
    } 

    async fn choose_random_item(&self, file_name: String) -> Result<String, Box<dyn Error>> {
        let fill_in_file = self.load_fill_in_file(file_name).await?;
        let item = fill_in_file.choose(&mut thread_rng()).to_owned().unwrap().to_string();
        Ok(item)
    }

    async fn interpolate(&self) -> Result<String, Box<dyn Error>> {
        let regex = r#"\{[a-z_\-]+\}"#; // regex? really?
        let mut interpolated = self.prompt.clone();
        let regex = Regex::new(regex).expect("Invalid regex for interpolating");
        for m in regex.find_iter(&self.prompt) {
            let item = m.as_str();
            let fill_in_file = m.as_str()
                .strip_prefix("{").unwrap()
                .strip_suffix("}").unwrap();
            let value = self.choose_random_item(fill_in_file.into()).await?;
            interpolated = interpolated.replacen(item, &value, 1);
        }
        Ok(interpolated)
    }
}

impl From<Vec<&str>> for Prompt {
    fn from(value: Vec<&str>) -> Self {
        Prompt {
            prompt_type: value[0].trim().into(),
            prompt: value[1].trim().into(),
        }
    }
}

async fn load_prompts() -> Result<Vec<Prompt>, Box<dyn Error>> {
    let mut prompts: Vec<Prompt> = vec![];
    let prompts_path = "resources/prompts.txt";
    let file = File::open(prompts_path).await?;
    let buf_reader = BufReader::new(file);
    let mut lines = buf_reader.lines();
    while let Some(line) = lines.next_line().await? {
        if !line.starts_with("#") {
            let splitted_line: Vec<_> = line.split(";").collect();
            if splitted_line.len() == 2 {
                prompts.push(splitted_line.into())
            }
        }
    }
    Ok(prompts)
}

async fn query_chat_gpt(model: String, prompt: String) -> Result<String, Box<dyn Error>> {
    let client = Client::new();
    let prompt = prompt.clone();
    let request = CreateChatCompletionRequestArgs::default()
    .max_tokens(768u16)
    .model(model)
    .messages([
        ChatCompletionRequestMessageArgs::default()
            .role(Role::System)
            .content(prompt)
            .build()?
    ])
    .build()?;
    let response = client.chat().create(request).await?;
    let first_response = response.choices.get(0).ok_or("No first item in response")?;
    let first_response = first_response.message.content.to_owned();
    Ok(first_response)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let prompts = load_prompts().await?;
    let prompt = prompts.choose(&mut thread_rng()).unwrap();
    let prompt_text = &prompt.prompt;
    let interpolated_prompt = prompt.interpolate().await?;
    let full_prompt = format!("Respond only with JSON with 'text' field. {}", interpolated_prompt);
    println!("Full prompt: {:#?}", &full_prompt);
    let model = if thread_rng().gen_bool(0.9) {
        "gpt-3.5-turbo"
    } else {
        "gpt-4"
    }.to_string();
    println!("GPT model: {}", &model);
    let response = query_chat_gpt(model.clone(), full_prompt).await?;
    println!("Response JSON from ChatGPT\n{}", &response);
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&response).unwrap();
    let text = map["text"].as_str().expect("Cannot parse JSON!");
    let censor = Censor::Standard + Censor::Sex - "ex" - "sex";
    let text = censor.replace_with_offsets(&text, "*", 1, 0);
    println!("Parsed and censored text:\n\n{}\n", &text);

    let debug_info = format!(r#"
═══════════════
🤖 {model}
❓ {prompt_text}
❗ {interpolated_prompt}
    "#); 

    println!("DEBUG INFO TO POST \n{}", debug_info);
    Ok(())
}
