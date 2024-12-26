use helix_core::{Tendril, Transaction};
use helix_lsp::block_on;

use crate::{
    commands::LINE_ENDING_REGEX,
    compositor,
    ui::{self, PromptEvent},
};

use super::{exit_select_mode, Context};

pub fn llm_replace(cx: &mut Context) {
    fn create_llm_prompt(history_register: Option<char>) -> Box<ui::Prompt> {
        let prompt = ui::Prompt::new(
            "prompt:".into(),
            history_register,
            ui::completers::none,
            move |cx: &mut compositor::Context, input: &str, event: PromptEvent| {
                if event != PromptEvent::Validate {
                    return;
                }
                let scrolloff = cx.editor.config().scrolloff;
                let (view, doc) = current!(cx.editor);
                let text = doc.text().slice(..);
                let map_value = |value: &str| {
                    let value = LINE_ENDING_REGEX.replace_all(&value, doc.line_ending.as_str());
                    Tendril::from(value.as_ref())
                };
                let res: Result<Vec<String>> = doc
                    .selection(view.id)
                    .fragments(text)
                    .map(|code| block_on(get_completion(input, &code)))
                    .collect();
                let mut values = match res {
                    Ok(values) => values.into_iter().map(|v| map_value(&v)),
                    Err(e) => {
                        cx.editor.set_status(format!("llm invocation failed: {e}"));
                        return;
                    }
                };

                let selection = doc.selection(view.id);
                let transaction =
                    Transaction::change_by_selection(doc.text(), selection, |range| {
                        if !range.is_empty() {
                            (range.from(), range.to(), values.next())
                        } else {
                            (range.from(), range.to(), None)
                        }
                    });
                drop(values);

                let (view, doc) = current!(cx.editor);
                doc.apply(&transaction, view.id);
                doc.append_changes_to_history(view);
                view.ensure_cursor_in_view(doc, scrolloff);
                cx.editor
                    .set_status(format!("text replaced using llm prompt \"{input}\""));
            },
        );

        Box::new(prompt)
    }

    let history_register = cx.register;
    let prompt = create_llm_prompt(history_register);
    cx.push_layer(prompt);
    exit_select_mode(cx);
}

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::{json, Value};
use std::env;

pub async fn get_completion(prompt: &str, code: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let api_key = env::var("ANTHROPIC_API_KEY")?;
    let model =
        env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| String::from("claude-3-5-sonnet-20241022"));

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert("X-API-Key", HeaderValue::from_str(&api_key)?);
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

    let system_prompt = "You are a code modification assistant. Your task is to rewrite code \
                        according to given instructions. Always return only the modified code, \
                        wrapped in triple backticks. Do not include any explanations or \
                        additional text. Keep the same indent levels as in the original code.";

    let user_prompt = format!(
        "Instructions for code modification:\n{}\n\n\
         Original code:\n\
         ```\n\
         {}\n\
         ```\n\n\
         Provide only the modified code wrapped in triple backticks. No explanations needed. \
         Keep the same indent levels as in the original code.",
        prompt, code
    );

    let body = json!({
        "model": model,
        "max_tokens": 4096,
        "temperature": 0.05,
        "system": system_prompt,
        "stop_sequences": ["\n```"],
        "messages": [
            {
                "role": "user",
                "content": user_prompt
            },
            {
                "role": "assistant",
                "content": "```"
            },
        ],
    });

    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .headers(headers)
        .json(&body)
        .send()
        .await?;

    let response_body: Value = response.json().await?;
    let raw_completion = response_body["content"][0]["text"]
        .as_str()
        .ok_or_else(|| {
            anyhow::anyhow!("Failed to get completion text from response: {response_body}")
        })?;

    // Extract code between backticks, preserving indentation and newlines
    let completion = raw_completion.trim_matches(['`'; 3]);
    Ok(completion.trim_start_matches('\n').to_string())
}
