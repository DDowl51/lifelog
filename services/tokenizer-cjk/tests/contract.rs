#![allow(clippy::unwrap_used)]

use tokenizer_cjk::TokenizerCjk;
use tokenizer_contract::TOKENIZER;
use tokenizer_contract::testing::tokenizer_contract;

#[tokio::test]
async fn passes_the_contract() {
    let app = testkit::app().with(TokenizerCjk).boot().await.unwrap();
    tokenizer_contract(app.resolve(&TOKENIZER).unwrap())
        .await
        .unwrap();
}
