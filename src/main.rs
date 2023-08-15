use async_openai::{
    types::{
        ChatCompletionFunctionsArgs, ChatCompletionRequestMessageArgs,
        CreateChatCompletionRequestArgs, Role, ChatCompletionFunctions,
    },
    Client,
};
use censor::Censor;

use image2::{
    text::{load_font, width},
    *,
};
use rand::prelude::*;
use regex::Regex;
use reqwest::{
    multipart::{self},
    Body,
};
use serde_json::{self, json, Map, Value};
use std::{env, error::Error};
use tokio::{
    fs::*,
    io::{AsyncBufReadExt, BufReader},
};
use tokio_util::codec::{BytesCodec, FramedRead};

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
        let item = fill_in_file
            .choose(&mut thread_rng())
            .to_owned()
            .unwrap()
            .to_string();
        Ok(item)
    }

    async fn interpolate(&self) -> Result<String, Box<dyn Error>> {
        let regex = r#"\{[a-z_\-]+\}"#; // regex? really?
        let mut interpolated = self.prompt.clone();
        let regex = Regex::new(regex).expect("Invalid regex for interpolating");
        for m in regex.find_iter(&self.prompt) {
            let item = m.as_str();
            let fill_in_file = m
                .as_str()
                .strip_prefix("{")
                .unwrap()
                .strip_suffix("}")
                .unwrap();
            let value = self.choose_random_item(fill_in_file.into()).await?;
            interpolated = interpolated.replacen(item, &value, 1);
        }
        Ok(interpolated)
    }

    fn get_function_call(&self) -> Value {
        match &self.prompt_type {
            PromptType::Text => json!({"name":"set_text"}),
            PromptType::Image => json!({"name":"set_meme"}),
        }
    }

    fn get_functions(&self) -> Vec<ChatCompletionFunctions> {
        match &self.prompt_type {
            PromptType::Text => {
                vec![
                    ChatCompletionFunctionsArgs::default()
                        .name("set_text")
                        .description("Set the current text to display to user")
                        .parameters(json!({
                            "type": "object",
                            "properties": {
                                "text": {
                                    "type": "string",
                                }
                            },
                            "required": ["text"],
                        }))
                        .build().unwrap()
                ]
            }
            PromptType::Image => {
                vec![
                    ChatCompletionFunctionsArgs::default()
                        .name("set_meme")
                        .description("Set the current meme to display to user")
                        .parameters(json!({
                            "type": "object",
                            "properties": {
                                "top_text": {
                                    "type": "string",
                                },
                                "bottom_text": {
                                    "type": "string",
                                }                                
                            },
                            "required": ["top_text", "bottom_text"],
                        }))
                        .build().unwrap()
                ]
            }
        }
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

async fn query_chat_gpt(model: String, prompt: String, functions: Vec<ChatCompletionFunctions>, function_call: Value) -> Result<Value, Box<dyn Error>> {
    let proxy = reqwest::Proxy::all("http://127.0.0.1:8080")?;
    let http_client = reqwest::Client::builder().proxy(proxy).build()?;
    let client = Client::new().with_http_client(http_client);
    let request = CreateChatCompletionRequestArgs::default()
        .max_tokens(768u16)
        .model(model)
        .functions(functions)
        .messages([ChatCompletionRequestMessageArgs::default()
            .role(Role::User)
            .content(prompt)
            .build()?])
        .function_call(function_call)
        .build()?;
    let response = client.chat().create(request).await?;
    let first_response = response.choices.get(0).ok_or("No first item in response")?;
    let function_call = first_response.message.function_call.clone().unwrap();
    let function_args: Value = function_call.arguments.parse().unwrap();
    Ok(function_args)
}

async fn send_mastodon_image(image_path: String) -> Result<String, Box<dyn Error>> {
    let instance = env::var("MAST_INSTANCE")?;
    let token = env::var("MAST_TOKEN")?;
    let url = format!("https://{instance}/api/v2/media");
    let client = reqwest::Client::new();
    let file = File::open(image_path.clone()).await?;
    let stream = FramedRead::new(file, BytesCodec::new());
    let file_body = Body::wrap_stream(stream);
    let some_file = multipart::Part::stream(file_body)
        .file_name(image_path)
        .mime_str("image/jpeg")?;
    let form = multipart::Form::new().part("file", some_file);
    let response = client
        .post(url)
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await?;
    let text = response.text().await?;
    println!("{}", &text);
    let obj: Map<String, Value> = serde_json::from_str(&text)?;
    let id = &obj["id"].as_str().unwrap();

    Ok(id.to_string())
}

async fn send_mastodon_msg(
    text: String,
    image_id: Option<String>,
) -> Result<String, Box<dyn Error>> {
    let params = [
        ("status", text.clone()),
        ("visibility", "public".to_owned()),
        ("language", "en".to_owned()),
        ("media_ids[]", image_id.unwrap_or(String::new())),
    ];

    let instance = env::var("MAST_INSTANCE")?;
    let token = env::var("MAST_TOKEN")?;
    let url = format!("https://{instance}/api/v1/statuses");
    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .bearer_auth(token)
        .form(&params)
        .send()
        .await?;
    let text = response.text().await?;
    Ok(text)
}

fn get_image_from_prompt(prompt: &str) -> String {
    let matches: Vec<_> = prompt.match_indices('"').collect();
    let (first_index, _) = matches[0];
    let (last_index, _) = matches[1];
    let between_quotes = &prompt[first_index + 1..last_index];
    between_quotes.to_owned()
}

fn generate_image(
    image_name: &str,
    top_text: &str,
    bottom_text: &str,
) -> Result<String, Box<dyn Error>> {
    let font = load_font("font/Anton-Regular.ttf")?;
    let image_name = format!("images/{}.jpg", image_name);
    let mut image = Image::<f32, Rgb>::open(image_name)?;
    let size = 55.0_f32;
    let image_width = image.size().width;
    let image_height = image.size().height;

    for offset in 0..=4 {
        let offset = 4 - offset;
        let px: Pixel<Rgb> = if offset != 0 {
            Pixel::from(vec![1.0_f64, 1.0, 1.0])
        } else {
            Pixel::from(vec![0.0_f64, 0.0, 0.0])
        };

        let text_width = width(&top_text, &font, size);
        let x = if text_width < image_width {
            (image_width - text_width) / 2
        } else {
            0
        };
        image.draw_text(
            top_text,
            &font,
            size,
            (x + offset, size as usize + offset),
            &px,
        );

        let text_width = width(&bottom_text, &font, size);
        let x = if text_width < image_width {
            (image_width - text_width) / 2
        } else {
            0
        };
        image.draw_text(
            bottom_text,
            &font,
            size,
            (x + offset, image_height - 20 + offset),
            &px,
        );
    }

    let image_name = "output/output.jpg";
    image.save(image_name)?;
    Ok(image_name.to_owned())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let prompts = load_prompts().await?;
    let prompt = prompts.choose(&mut thread_rng()).unwrap();
    let prompt_text = &prompt.prompt;
    let interpolated_prompt = prompt.interpolate().await?;
    println!("Prompt: {:#?}", &interpolated_prompt);
    let model = if thread_rng().gen_bool(0.95) {
        "gpt-3.5-turbo"
    } else {
        "gpt-4"
    }
    .to_string();
    println!("GPT model: {}", &model);
    let response = query_chat_gpt(model.clone(), interpolated_prompt.clone(), prompt.get_functions(), prompt.get_function_call()).await?;
    let censor = Censor::Standard + Censor::Sex - "sex" - "ass";

    let debug_info = format!(
        r#"
    ══════════════
    🤖 {model}
    ❓ {prompt_text}
    ❗ {interpolated_prompt}"#
    );

    println!("\n\nDEBUG INFO TO POST \n{}", &debug_info);

    println!("Response JSON from ChatGPT\n{}", &response);
    match prompt.prompt_type {
        PromptType::Text => {
            let text = censor.replace_with_offsets(&response["text"].as_str().unwrap(), "*", 1, 0);
            println!("Text post:\n{}", &text);
            send_mastodon_msg(text, None).await?;

            println!("Mastodon message sent!!!");
        }
        PromptType::Image => {
            let top_text = &response["top_text"].as_str().unwrap();
            let top_text = censor.replace_with_offsets(&top_text, "*", 1, 0);
            let bottom_text = &response["bottom_text"].as_str().unwrap();
            let bottom_text = censor.replace_with_offsets(&bottom_text, "*", 1, 0);
            let image_meme = get_image_from_prompt(&interpolated_prompt);
            println!("Image meme");
            println!("TOP TEXT    : {}", &top_text);
            println!("BOTTOM TEXT : {}", &bottom_text);
            println!("IMAGE       : {}", &image_meme);
            println!("Generating image");
            let image_file = generate_image(&image_meme, &top_text, &bottom_text)?;
            println!("Image file: {}", &image_file);
            let image_id = send_mastodon_image(image_file).await?;
            println!("Posted image file, and got its id: {}", &image_id);

            send_mastodon_msg("".to_owned(), image_id.into()).await?;

            println!("Mastodon message sent!!!");
        }
    }

    Ok(())
}
