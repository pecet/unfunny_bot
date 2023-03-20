use std::{error::Error, env};
use async_openai::{types::{CreateChatCompletionRequestArgs, ChatCompletionRequestMessageArgs, Role}, Client};
use tokio::{fs::*, io::{BufReader, AsyncBufReadExt}};
use rand::prelude::*;
use regex::Regex;
use censor::Censor;
use serde_json;
use image2::{*, text::{font, load_font, width}};

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

    fn prompt_json(&self) -> String {
        match self.prompt_type {
            PromptType::Text => "Respond only with JSON with 'text' field.",
            PromptType::Image => "Respond only with JSON with 'top_text' and 'bottom_text' for image meme macro.",
        }.to_owned()
    }

    fn parse_json(&self, response: &str) -> Vec<String> {
        let mut vec: Vec<String> = vec![];
        match self.prompt_type {
            PromptType::Text => {
                let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(response).unwrap();
                let text = map["text"].as_str().expect("Cannot parse JSON!");
                vec.push(text.to_owned()); 
            },
            PromptType::Image => {
                let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(response).unwrap();
                let bottom_text = map["bottom_text"].as_str().expect("Cannot parse bottom text from JSON!");
                let top_text = map["top_text"].as_str().expect("Cannot parse top text from JSON!");
                vec.push(top_text.to_owned());
                vec.push(bottom_text.to_owned());
            },
        }
        vec
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

async fn send_mastodon_msg(text: String) -> Result<String, Box<dyn Error>> {
    let params = [
        ("status", text.clone()),
        ("visibility", "public".to_owned()),
        ("language", "en".to_owned()),
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
    let (first_index,_) = matches[0];
    let (last_index,_) = matches[1];
    let between_quotes = &prompt[first_index+1..last_index];
    between_quotes.to_owned()
}

fn generate_image(image_name: &str, top_text: &str, bottom_text: &str) -> Result<String, Box<dyn Error>> {
    let font = load_font("font/Anton-Regular.ttf")?;
    let image_name = format!("images/{}.jpg", image_name);
    let mut image = Image::<f32, Rgb>::open(image_name)?;
    let px: Pixel<Rgb> = Pixel::from(vec![1.0_f64, 1.0, 1.0]);
    let size = 55.0_f32;
    let image_width = image.size().width;
    let image_height = image.size().height;

    let text_width = width(&top_text, &font, size);
    let x = (image_width - text_width) / 2;
    image.draw_text(top_text, &font, size, (x, size as usize), &px);

    let text_width = width(&bottom_text, &font, size);
    let x = (image_width - text_width) / 2;
    image.draw_text(bottom_text, &font, size, (x, image_height - 20), &px);

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
    let full_prompt = format!("{} {}", prompt.prompt_json(), interpolated_prompt);
    println!("Full prompt: {:#?}", &full_prompt);
    let model = if thread_rng().gen_bool(0.9) {
        "gpt-3.5-turbo"
    } else {
        "gpt-4"
    }.to_string();
    println!("GPT model: {}", &model);
    let response = query_chat_gpt(model.clone(), full_prompt.clone()).await?;
    let censor = Censor::Standard + Censor::Sex - "sex" - "ass";

    println!("Response JSON from ChatGPT\n{}", &response);
    match prompt.prompt_type {
        PromptType::Text => {
            let text = &prompt.parse_json(&response)[0];
            let text = censor.replace_with_offsets(&text, "*", 1, 0);
            println!("Text post:\n{}", &text);
        },
        PromptType::Image => {
            let top_text = &prompt.parse_json(&response)[0];
            let top_text = censor.replace_with_offsets(&top_text, "*", 1, 0);
            let bottom_text = &prompt.parse_json(&response)[1];
            let bottom_text = censor.replace_with_offsets(&bottom_text, "*", 1, 0);
            let image_meme = get_image_from_prompt(&full_prompt);
            println!("Image meme");
            println!("TOP TEXT    : {}", &top_text);
            println!("BOTTOM TEXT : {}", &bottom_text);
            println!("IMAGE       : {}", &image_meme);
            println!("Generating image");
            let image_file = generate_image(&image_meme, &top_text, &bottom_text)?;
            println!("Image file: {}", &image_file);
        },
    }


//     let debug_info = format!(r#"
// ═══════════════
// 🤖 {model}
// ❓ {prompt_text}
// ❗ {interpolated_prompt}
//     "#);

//     println!("\n\nDEBUG INFO TO POST \n{}", &debug_info);

//     let message_to_send = format!("{text}\n\n{debug_info}");
//     send_mastodon_msg(message_to_send).await?;

//     println!("Mastodon message sent!!!");


    Ok(())
}
