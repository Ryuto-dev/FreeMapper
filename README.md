# FreeMapper

MadMapperのオープンソース版を目指すプロジェクションマッピング＋映像Hub。SPECは `docs/SPEC-v0.1.md`。

- 理念: プロ品質を無料で、気に入ったら寄付 (DaVinci Resolve型)
- License: AGPL-3.0-or-later
- Stack: Rust + wgpu (via eframe) + egui + FFmpeg (sidecar方式・要`brew install ffmpeg`)
- i18n: 日本語/英語 (`locales/`)

## 開発

```sh
cargo test --workspace
cargo run -p freemapper-app
```

v0.1内容: Quad/Mesh/Mask + 画像/GIF + ジェネレーター6種 + エフェクト + 出力ウィンドウ + プリセット保存 + Syphon出力(macOS)。

## macOS: Syphon出力の前提

Syphon.frameworkが必要です (OBS/MadMapper等と共有する仕組み本体)。

```sh
# 公式SDK (x86_64のみ) または Syphon対応アプリ同梱のuniversal版を使用
# 例: OBS同梱版 (x86_64+arm64) をユーザFrameworksへ
cp -R "/Applications/OBS.app/Contents/Frameworks/Syphon.framework" ~/Library/Frameworks/
# @rpath解決のためIDを絶対パス化 (開発機のみ)
install_name_tool -id ~/Library/Frameworks/Syphon.framework/Versions/A/Syphon \
  ~/Library/Frameworks/Syphon.framework/Versions/A/Syphon
```

配布時は公式SDK (https://github.com/Syphon/Syphon-Framework/releases) から取得すること
(Syphon自体はBSDライセンス、AGPL本体とは動的リンクで共存)。
