// use rig::{providers::openai::OpenAI, tool::ToolExecutionError};
use rig::tool::ToolExecutionError;

#[rig::rig_tool(description = "Count the words in the supplied text")]
fn count_words(text: String) -> Result<usize, ToolExecutionError> {
    Ok(text.split_whitespace().count())
}

use rig::prelude::*;
// use rig::providers::openai;
use rig::providers::openrouter;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load OPENAI_API_KEY from a .env file (if present) into the environment.
    // Load OPENROUTER_API_KEY from a .env file (if present) into the environment.
    dotenvy::dotenv().ok();

    // let model = OpenAI::from_env()?.completion(openai::GPT_6_LUNA);
    // Any OpenRouter model id works here; see https://openrouter.ai/models
    let model = openrouter::from_env()?.completion("openai/gpt-6-luna");
    let agent = AgentBuilder::new(model)
        .preamble(
            "You are a helpful Rust AI agent. \
             Use the count_words tool when the user asks \
             you to count words. Explain the result clearly.",
        )
        .tool(CountWords)
        .build();
    let answer = agent
        .prompt(
            "Use the count_words tool to count the words \
             in this sentence: Rust makes AI infrastructure safer.",
        )
        .max_turns(4)
        .await?
        .output();
    println!("Agent: {answer}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_words_separated_by_whitespace() {
        assert_eq!(
            count_words("Rust makes AI infrastructure safer.".to_string()).unwrap(),
            5
        );
    }
    #[test]
    fn handles_empty_text() {
        assert_eq!(count_words(String::new()).unwrap(), 0);
    }
}
