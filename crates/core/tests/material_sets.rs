use publisher_core::{Asset, MaterialSet, Store};
use std::{fs, path::PathBuf};

fn file(root: &tempfile::TempDir, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
    let path = root.path().join(name);
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn mixed_import_persists_one_set_and_keeps_existing_asset_ids_and_order() {
    let dir = tempfile::tempdir().unwrap();
    let legacy_video = Asset::inspect(&file(&dir, "旧视频.mp4", b"legacy video")).unwrap();
    let legacy_images = Asset::inspect_images(&[
        file(&dir, "旧图二.jpg", b"legacy image 2"),
        file(&dir, "旧图一.png", b"legacy image 1"),
    ])
    .unwrap();
    let old_db = rusqlite::Connection::open(dir.path().join("publisher.sqlite3")).unwrap();
    old_db
        .execute_batch(include_str!("../../../migrations/001_initial.sql"))
        .unwrap();
    let mut old_video = serde_json::to_value(&legacy_video).unwrap();
    old_video.as_object_mut().unwrap().remove("kind");
    for (id, data) in [
        (&legacy_video.id, old_video),
        (
            &legacy_images.id,
            serde_json::to_value(&legacy_images).unwrap(),
        ),
    ] {
        old_db
            .execute("INSERT INTO assets VALUES(?1,?2)", (id, data.to_string()))
            .unwrap();
    }
    drop(old_db);

    let store = Store::open(dir.path()).unwrap();
    let video_1 = file(&dir, "视频一.MOV", b"first video");
    let video_2 = file(&dir, "视频二.mp4", b"second video");
    let image_1 = file(&dir, "首图.PNG", b"first image");
    let image_2 = file(&dir, "尾图.webp", b"second image");
    let copy = file(
        &dir,
        "作品.md",
        "\u{feff}\r\n# 发布标题\r\n第一行\r\n\r\n第二行\r\n",
    );
    let imported = store
        .import_material_set(&[
            video_2.clone(),
            image_2.clone(),
            copy.clone(),
            video_1.clone(),
            image_1.clone(),
        ])
        .unwrap();
    assert_eq!(imported.title, "发布标题");
    assert_eq!(imported.description, "第一行\n\n第二行");
    assert_eq!(imported.text_files, [copy.canonicalize().unwrap()]);
    assert_eq!(
        imported
            .videos
            .iter()
            .map(|asset| asset.path.clone())
            .collect::<Vec<_>>(),
        [
            video_2.canonicalize().unwrap(),
            video_1.canonicalize().unwrap()
        ]
    );
    assert_eq!(
        imported
            .images
            .as_ref()
            .unwrap()
            .images
            .iter()
            .map(|image| image.path.clone())
            .collect::<Vec<_>>(),
        [
            image_2.canonicalize().unwrap(),
            image_1.canonicalize().unwrap()
        ]
    );

    let reopened = Store::open(dir.path()).unwrap();
    let sets = reopened.material_sets().unwrap();
    assert_eq!(sets.len(), 3);
    assert_eq!(sets[0], imported);
    assert_eq!(sets[1].id, legacy_images.id);
    assert_eq!(sets[1].images.as_ref(), Some(&legacy_images));
    assert_eq!(sets[2].id, legacy_video.id);
    assert_eq!(sets[2].videos, [legacy_video]);
    let assets = reopened.assets().unwrap();
    assert_eq!(assets.len(), 5);
    assert!(imported.assets().all(|asset| assets.contains(asset)));
}

#[test]
fn text_only_sets_use_labelled_files_and_keep_long_plain_paragraphs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let description = file(
        &dir,
        "项目-description.markdown",
        "简介：这是正文\r\n\r\n第二段",
    );
    let title = file(
        &dir,
        "项目-标题.txt",
        "\u{feff}标题：\r\n单独的作品标题\r\n",
    );
    let labelled = store.import_material_set(&[description, title]).unwrap();
    assert_eq!(labelled.title, "单独的作品标题");
    assert_eq!(labelled.description, "这是正文\n\n第二段");
    assert_eq!(labelled.assets().count(), 0);

    let body = format!("{}\n尾段", "长".repeat(81));
    let document = file(&dir, "未分段文案.txt", &body);
    let unlabelled = store.import_material_set(&[document]).unwrap();
    assert_eq!(unlabelled.title, "未分段文案");
    assert_eq!(unlabelled.description, body);
    assert_eq!(store.material_sets().unwrap(), [unlabelled, labelled]);
    assert!(store.assets().unwrap().is_empty());
}

#[test]
fn unambiguous_video_name_identifies_the_main_video() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let alternate = file(&dir, "花絮.mp4", b"alternate");
    let primary = file(&dir, "02 主体.mp4", b"main video");
    let image = file(&dir, "附图.png", b"image");
    let set = store
        .import_material_set(&[image, alternate.clone(), primary.clone()])
        .unwrap();
    assert_eq!(set.title, "02 主体");
    assert_eq!(set.videos[0].path, primary.canonicalize().unwrap());
    assert_eq!(set.videos[1].path, alternate.canonicalize().unwrap());

    let ambiguous = file(&dir, "正片.mov", b"another video");
    let set = store
        .import_material_set(&[alternate.clone(), primary, ambiguous])
        .unwrap();
    assert_eq!(set.videos[0].path, alternate.canonicalize().unwrap());
}

#[test]
fn invalid_imports_do_not_leave_partial_materials_or_assets() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let video = file(&dir, "视频.mp4", b"first video");
    let unsupported = file(&dir, "附件.zip", b"unsupported");
    let invalid_encoding = file(&dir, "编码.txt", [0xff, 0xfe]);
    let oversized = file(&dir, "过长.md", vec![b'x'; 64 * 1024 + 1]);
    for (invalid, expected) in [
        (unsupported, "不支持导入"),
        (dir.path().join("./视频.mp4"), "不能重复导入"),
        (invalid_encoding, "UTF-8"),
        (oversized, "64 KB"),
    ] {
        let error = store
            .import_material_set(&[video.clone(), invalid])
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        assert!(store.assets().unwrap().is_empty());
        assert!(store.material_sets().unwrap().is_empty());
    }

    let db = rusqlite::Connection::open(dir.path().join("publisher.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_second_asset BEFORE INSERT ON material_set_assets
         WHEN (SELECT count(*) FROM material_set_assets) > 0
         BEGIN SELECT RAISE(ABORT, 'test transaction rollback'); END;",
    )
    .unwrap();
    let second_video = file(&dir, "另一个视频.webm", b"second video");
    assert!(store.import_material_set(&[video, second_video]).is_err());
    assert!(store.assets().unwrap().is_empty());
    assert_eq!(store.material_sets().unwrap(), Vec::<MaterialSet>::new());
}
