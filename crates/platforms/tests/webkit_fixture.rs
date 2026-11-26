#![cfg(target_os = "macos")]

use publisher_platforms::webkit::{Session, remove_profile};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

#[tokio::test]
async fn native_webkit_upload_input_and_isolation() -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/", listener.local_addr()?);
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0; 4096];
                let length = stream.read(&mut request).await.unwrap_or(0);
                let request_text = String::from_utf8_lossy(&request[..length]);
                if request_text.starts_with("POST /publish ")
                    || request_text.starts_with("POST /unrelated ")
                {
                    let body = r#"{"result":"local fixture"}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    return;
                }
                let body = r#"<!doctype html><meta charset=utf-8>
                    <button style="position:absolute;left:40px;top:30px;width:130px;height:60px"
                        onclick="window.trusted=event.isTrusted">Click</button>
                    <input id="files" type="file" multiple hidden>
                    <input id="edit">
                    <script>
                    files.onchange = async () => {
                        window.selected = await Promise.all([...files.files].map(async f => [f.name, await f.text()]));
                    };
                    edit.addEventListener('keydown', event => {if(event.key === 'Enter') window.entered = true;});
                    </script>"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    let first_id = Uuid::new_v4();
    let second_id = Uuid::new_v4();
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("fixture.txt");
    std::fs::write(&file, "local media fixture")?;
    let result: anyhow::Result<()> = async {
        let first = Session::launch(first_id, true).await?;
        let second = Session::launch(second_id, true).await?;
        first.navigate(&url).await?;
        second.navigate(&url).await?;
        first
            .evaluate::<Value>("localStorage.setItem('marker','first'); null")
            .await?;
        assert_eq!(
            second
                .evaluate::<Option<String>>("localStorage.getItem('marker')")
                .await?,
            None
        );

        first.click(90., 50.).await?;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(first.evaluate::<bool>("window.trusted").await?);
        assert!(
            first
                .choose_files(
                    "document.querySelector('#missing')",
                    std::slice::from_ref(&file)
                )
                .await
                .is_err()
        );
        first
            .choose_files(
                "document.querySelector('#files')",
                std::slice::from_ref(&file),
            )
            .await?;
        let selected = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Ok(Some(value)) = first
                    .evaluate::<Option<Value>>("window.selected ?? null")
                    .await
                {
                    break value;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await?;
        assert_eq!(
            selected,
            serde_json::json!([["fixture.txt", "local media fixture"]])
        );

        first
            .evaluate::<Value>("document.getElementById('edit').focus(); null")
            .await?;
        first.insert_text("Airs").await?;
        assert_eq!(
            first
                .evaluate::<String>("document.getElementById('edit').value")
                .await?,
            "Airs"
        );
        first.press_key("Enter").await?;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(first.evaluate::<bool>("window.entered === true").await?);
        let image = first.screenshot(Some([20., 20., 300., 200.])).await?;
        assert!(image.starts_with(b"\x89PNG\r\n\x1a\n"));

        // Custom app links must not leave the WebKit session or show an OS prompt.
        // Exercise redirects, iframes, and new-window link targets.
        first
            .evaluate::<Value>(
                r#"(() => {
            const frame = document.createElement('iframe');
            frame.src = 'bitbrowser://cc/';
            document.body.append(frame);
            const link = document.createElement('a');
            link.href = 'bitbrowser://cc/';
            link.target = '_blank';
            document.body.append(link);
            link.click();
            location.href = 'bitbrowser://cc/';
            return null;
        })()"#,
            )
            .await?;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(first.evaluate::<String>("location.href").await?, url);
        assert_eq!(
            first
                .evaluate::<String>("document.querySelector('#edit').value")
                .await?,
            "Airs"
        );
        // Ordinary HTTPS and same-origin navigation must remain available.
        first
            .evaluate::<Value>("location.hash = 'fixture'; null")
            .await?;
        assert!(
            first
                .evaluate::<bool>("location.hash === '#fixture'")
                .await?
        );

        let endpoint = format!("{}publish", url);
        first.arm_receipts(std::slice::from_ref(&endpoint)).await?;
        first
            .evaluate::<Value>("fetch('/unrelated',{method:'POST',body:'ignore'}).then(()=>null)")
            .await?;
        first
            .evaluate::<Value>("fetch('/publish',{method:'POST',body:'expected'}).then(()=>null)")
            .await?;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let receipts = first.take_receipts().await?;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].endpoint, endpoint);
        assert_eq!(receipts[0].request.as_deref(), Some("expected"));
        assert_eq!(receipts[0].status, 200);
        assert_eq!(
            serde_json::from_str::<Value>(&receipts[0].body)?["result"],
            "local fixture"
        );
        assert!(first.take_receipts().await?.is_empty());

        first.close().await?;
        let restored = Session::launch(first_id, true).await?;
        restored.navigate(&url).await?;
        assert_eq!(
            restored
                .evaluate::<String>("localStorage.getItem('marker')")
                .await?,
            "first"
        );
        restored.close().await?;
        second.close().await?;
        Ok(())
    }
    .await;
    server.abort();
    let first_cleanup = remove_profile(first_id).await;
    let second_cleanup = remove_profile(second_id).await;
    result?;
    first_cleanup?;
    second_cleanup?;
    Ok(())
}

#[tokio::test]
async fn douyin_work_list_requires_unique_new_published_content() -> anyhow::Result<()> {
    let session = Session::launch(Uuid::new_v4(), false).await?;
    session.navigate("about:blank").await?;
    let matcher = include_str!("../webkit/douyin-work-list.js");
    let criteria = serde_json::json!({
        "title": "测试视频", "description": "测试说明 #测试",
        "since": 1000, "until": 1200
    });
    let work = |id: Value, time: u64| {
        serde_json::json!({
            "aweme_id": id, "create_time": time, "item_title": "测试视频",
            "desc": "测试视频  测试说明 #测试", "aweme_type": 4, "status_value": 102,
            "status": {"is_delete": false, "is_private": false, "is_prohibited": false}
        })
    };
    let expected = work(serde_json::json!("1234567890123456789"), 1100);
    let mut wrong = work(serde_json::json!("1234567890123456790"), 1101);
    wrong["desc"] = "不同文案".into();
    let mut pending = work(serde_json::json!("1234567890123456791"), 1102);
    pending["status_value"] = 101.into();
    let mut private = work(serde_json::json!("1234567890123456792"), 1103);
    private["status"]["is_private"] = true.into();
    let rows = vec![
        work(serde_json::json!("1234567890123456793"), 999),
        work(serde_json::json!("1234567890123456794"), 1201),
        work(serde_json::json!(1234567890123456795u64), 1104),
        wrong,
        pending,
        private,
        expected.clone(),
    ];
    let payload = serde_json::json!({"status_code": 0, "aweme_list": rows});
    let result: Value = session
        .evaluate(&format!("({matcher})({criteria},{payload})"))
        .await?;
    assert_eq!(
        result,
        serde_json::json!([{"id":"1234567890123456789","createdAt":1100}])
    );
    let ambiguous = serde_json::json!({"status_code":0,"aweme_list":[expected,work(serde_json::json!("1234567890123456796"), 1105)]});
    let result: Vec<Value> = session
        .evaluate(&format!("({matcher})({criteria},{ambiguous})"))
        .await?;
    assert_eq!(result.len(), 2);
    let failed = serde_json::json!({"status_code":1,"aweme_list":[]});
    assert!(
        session
            .evaluate::<Value>(&format!("({matcher})({criteria},{failed})"))
            .await?
            .is_null()
    );
    session.close().await?;
    Ok(())
}
