//! 用真的模型测：要下载几百 MB，默认跳过。手动跑：
//! `LIFELOG_MODEL_CACHE=<目录> cargo test -p embed-fastembed -- --ignored`

#![allow(clippy::unwrap_used, clippy::expect_used)]

use embed_fastembed::EmbedFastembed;
use embedder_contract::{EMBEDDER, EmbedPurpose};
use kernel_api::ErrorKind;
use serde_json::json;

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[tokio::test]
#[ignore = "要下载模型"]
async fn the_real_model_puts_similar_meanings_close() {
    let cache =
        std::env::var("LIFELOG_MODEL_CACHE").unwrap_or_else(|_| "/tmp/lifelog-models".to_owned());
    let app = testkit::app()
        .with(EmbedFastembed)
        .config("embed-fastembed", json!({ "cache_dir": cache }))
        .unwrap()
        .boot()
        .await
        .unwrap();
    let embedder = app.resolve(&EMBEDDER).unwrap();
    // 贴近实际用法：问题换了说法，在摘要的段落里找（multilingual-e5-small 的相似度挤在 0.8～0.9，
    // vectors 的默认阈值 0.85 就是按这个量出来的）
    let passages = embedder
        .embed(
            vec![
                "weather-station：在 firmware 目录下烧录固件；排查串口设备，把用户加入 uucp 组以获取串口权限。".to_owned(),
                "周末去超市买菜，晚上做了红烧肉，和家人一起看电影。".to_owned(),
            ],
            EmbedPurpose::Passage,
        )
        .await
        .unwrap();
    let queries = embedder
        .embed(
            vec!["串口权限问题".to_owned(), "做饭".to_owned()],
            EmbedPurpose::Query,
        )
        .await
        .unwrap();
    assert_eq!(passages.vectors[0].len(), 384);
    let (work, life) = (&passages.vectors[0], &passages.vectors[1]);
    let (serial, cooking) = (&queries.vectors[0], &queries.vectors[1]);
    println!(
        "串口权限·工作 {:.3} 串口权限·生活 {:.3} 做饭·生活 {:.3} 做饭·工作 {:.3}",
        dot(serial, work),
        dot(serial, life),
        dot(cooking, life),
        dot(cooking, work)
    );
    assert!(
        dot(serial, work) > 0.85 && dot(serial, life) < 0.85,
        "相关的过阈值，无关的不过"
    );
    assert!(dot(cooking, life) > dot(cooking, work));
}

#[tokio::test]
async fn an_unknown_model_is_a_config_error() {
    let err = testkit::app()
        .with(EmbedFastembed)
        .config("embed-fastembed", json!({ "model": "gpt-embed" }))
        .unwrap()
        .boot()
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(err.message.contains("multilingual-e5-small"), "{err}");
}
