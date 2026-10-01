# FreeMapper SPEC v0.1

- Status: Draft
- Date: 2026-09-30
- Languages: 日本語 / English (初回リリースから両対応)

## 1. 理念 / Vision

- MadMapperのオープンソース版。プロと同じ操作感と品質を無料で。
- DaVinci Resolve型: 無料で本番使用可能、気に入ったら寄付 (Donationware)。
- 一切妥協しない設計: 描画精度・フレームレート・安定性を犠牲にしない。
- フル機能を見据えて設計、実装は段階的に。いきなり完成系は目指さない。
- 汎用性を高める: マッピング専用機ではなく、映像系ユーティリティ (Hub) としても使える。
  - 「このソフトを使えばネットワーク経由で映像を伝送できる」を標榜する。

3つの使い方:

1. Mapperモード: プロジェクションマッピング本機
2. Hubモード: 映像変換・中継器 (例: RTSP -> SRT, NDI -> RTMP)
3. Playerノード: RPi等を安価な受信・再生専用機に。多台分散出力・自動実行用。

## 2. ライセンス

- 本体: AGPLv3+ を採用。
  - 理由: 「勝手な製品化」抑止。GPLより強く、ネットワーク越し利用 (多台制御・スケジューラ) も還元対象になる。
  - デュアルライセンス・商用例外は設けない (寄付モデル維持)。
- 例外分離: プロプライエタリSDK連携 (NDI等) は本体に直リンクしない。
  - 別プロセスのオプションプラグイン + 動的ロード / IPC (TCP/共有メモリ) で分離。
  - 本体はクリーンなAGPLを維持。

## 3. 対応プラットフォーム・言語

- 対応OS (最終): macOS / Windows / Linux (x86_64 + ARM64 / Raspberry Pi)。
  - v0.1-v0.2: macOS + Windowsを優先。Linuxはビルドが通る状態を維持 (CI)。
  - v1.0: RPi公式対応 (`--player` ヘッドレス受信機として)。
- 対応言語 (初回から): 日本語 / 英語。
  - 文字列直書き禁止。全UI文言・エラー文・ドキュメントをキー管理。
  - `locales/ja.json`, `locales/en.json` + Fluent方式を想定。
  - フォント: Noto Sans JP同梱。IME確定・縦レイアウト崩れを最初からテスト項目化。

## 4. 技術スタック (推奨確定)

- Core言語: Rust (cargoで3OS統一ビルド、ARMクロスコンパイル容易)
  - 理由: 長時間自動実行・インスタレーションでのメモリ安全性。コントリビュータの暴走で落ちにくい。
- Renderer: wgpu (WebGPU) - Metal / DX12 / Vulkanを一本化。
  - OpenGL新規採用はしない (5年後に詰む)。RPi4以降のVulkanを想定。
  - カラーマネジメント・リニアワークフローを前提に設計。
- UI: egui (Immediate Mode)
  - MadMapper系のパラメータUIと相性良。Electronより軽量・低遅延。
  - JPフォント埋め込み・ダークテーマ標準。
- Media: FFmpeg (libav* via ffi)
  - HAP / ProRes対応必須 (プロ現場要件)。
  - HWデコード: VideoToolbox (macOS) / NVDEC・DXVA (Win) / V4L2M2M・VAAPI (Linux/RPi)。ゼロコピーでwgpuへ。
- Network伝送: SRT (libSR T, MPL: AGPL共存可) を中心に。RTSP/RTMPはFFmpeg libavformatで対応。
- Control: MIDI (midir), OSC (rosc), Art-Net/sACN自前実装、HTTP/MQTT (将来の多台同期用)。
- Fallback案: チームがC++勢なら C++20 + SDL3 + bgfx + Dear ImGui も可。ただし本SPECはRust前提で進める。

## 5. 全体アーキテクチャ

```
[Input: Stream I/O抽象レイヤー]
  File (mov/mp4/png/jpg) / Capture / TestPattern
  Network: RTSP Rx / RTMP Rx / SRT Rx-Tx / NDI* / Syphon* / Spout* (*=別プロセスPlugin)
  Shader: GLSL / ISF
    ↓
[Layer / Material] エフェクトチェーン・ブレンド・不透明度・カラコレ
    ↓
[Mapping] Quad / Mesh(Grid+Bezier) / Mask(Polygon) / 3D(後) / LED(後)
    ↓
[Output] Display Fullscreen xN / NDI Tx* / SRT/RTMP Push / Preview / Recorder
    ↓
[Control] MIDI / OSC / DMX-in / Keyboard + 将来: Scheduler / HTTP API / Clock Sync
```

原則:

- InputもOutputも `Stream` として抽象化。ファイルもネットワークも同じ `Source/Sink` trait。
- 具体プロトコルは後付け可能に。v0.1でtraitだけ切る。
- レンダーパイプラインはGPU常駐。CPUへのreadbackはRecorder以外禁止 (性能維持)。
- Project保存はJSON + schema version。後方互換マイグレーション必須。

## 6. 機能要件

### 6.1 Mapping (コア・妥協禁止領域)

- v0.1 MVP:
  - Quadワーピング (4点 + perspective補正)
  - Meshワーピング (Grid分割 + Bezierハンドル、最低8x8)
  - Mask (Polygon、Add/Subtract、Feather、Invert)
  - Slice (Layer -> Surface割付) の基本概念
- v0.2: ソフトエッジブレンディング、マルチ出力割付、スナップ・数値入力
- v0.3: 3D (glTF/OBJ読込、プロジェクターキャリブレーション、UV編集)、LEDマップ (fixture配置、Art-Net/sACN出力)

### 6.2 Media / Layer

- v0.1:
  - 画像 (png/jpg)、動画1系統 (H.264 + HAP優先、シーク・ループ・速度)
  - Shader簡易1本 (ISF互換サブセット or GLSLテンプレ)
  - Layer xN、Opacity・Blend・Solo/Mute
- v0.2:
  - エフェクトチェーン、NDI/Syphon/Spout入力、キャプチャ入力、Text
  - NDI送受信は別Pluginとして
- v0.3以降: Recorder (ProRes/H.264書出)、Playlist/Cue

### 6.3 Output / Performance

- v0.1: 単一Outputフルスクリーン + GUI内プレビュー、TestPattern、60fps維持 (M1 + Win iGPUで1080p2層)
- v0.2: マルチディスプレイ出力、Syphon/Spout/NDI送出、解像度・FPSモニタ
- 非機能目標: 4Kx2出力で60fps (dGPU時)、ドロップフレーム検出・自動画質劣化はしない (警告のみ、正直設計)

### 6.4 Network Hub (汎用性・本SPECの特徴)

- 抽象: `Source: File|Capture|R tsp|Rtmp|Srt|Ndi|Syphon|Spout|Generator` / `Sink: Display|RtmpPush|SrtPush|Ndi|Recorder`
- v0.2で実装: SRT Rx/Tx + RTSP受信 (IPカメラ取込)。RTMP送受信は互換用に付属。
- v0.3で実装: RTMP Push (配信ミラー)、SRT中継 (Hubモード: 入力->無変換パススルーで低CPU)
- 将来: WebRTC視野、SRT多台同期 (親PC -> RPi子)。
- 遅延明示: RTSP 0.5-2s / RTMP 2-5s / SRT 0.08-1s (設定依存) をUIに表示。リアルタイム用途との混同防止。

### 6.5 Control / Show / Automation

- v0.1: Shortcut・Preset保存 (JSON)、Undo/Redo
- v0.2: MIDI/OSC学習 (Learn)、DMX入力、Scene/Cue基本
- v1.0: Scheduler自動実行 (`--player` ヘッドレス、cron的タイムライン、起動時自動再生)、複数台同時制御 (OSC/HTTP API、Clock同期、MQTT検討)、RPi Player公式化

### 6.6 Utility

- Hubモードでの変換・中継プリセット、接続モニタ (bitrate・遅延・再送率)、ログ出力。
- インストール運用想定: 無操作復帰 (watchdog再起動)、リモート死活監視用HTTPヘルスチェック (将来)。

## 7. 非機能要件 (妥協しないライン)

- v0.1でも死守: ワーピング描画精度 (sub-pixel)、60fps維持、クラッシュしない保存形式 (atomic write + backup)。
- 起動<3秒、プロジェクト読込<2秒 (1080p基準)。
- 長時間稼働: 72h連続再生メモリリーク無し (CIで soak test)。
- セキュリティ: Network入力はタイムアウト・再接続・上限fgps必須。外部ストリームで落とさない。

## 8. UI/UX

- MadMapper踏襲: 左=ライブラリ/ソース、中央=ワークスペース/プレビュー、右=インスペクタ、下=レイヤー。
- ライブ編集第一: 出力中に全パラメータ変更可、出力が一瞬でも黒落ちしない。
- ショートカット全面対応、日英ショートカット表記切替。
- ダークテーマ標準、ハイDPI対応。

## 9. プロジェクトファイル

- `*.freemapper.json`、schema_version埋込。
- surfaces / layers / sources / outputs / controls を分離。
- 例 (略):
```json
{
  "schema": 1,
  "surfaces": [{"id":"s1","type":"quad","points":[[0,0],[1,0],[1,1],[0,1]]}],
  "layers": [{"id":"l1","source":"file://a.mov","surface":"s1"}]
}
```

## 10. ロードマップ

- v0.1 MVP: 1出力、画像/動画1系統、Quad+Mesh+Mask、Shader1本、日英切替、JSON保存、Stream traitのみ
- v0.2 Hub-I: マルチ出力、ソフトエッジ、SRT/RTSP受信、RTMP互換、Syphon/Spout/NDI(Plugin)、MIDI/OSC Learn
- v0.3 Stage: 3D + LED + ArtNet/sACN、RTMP/SRT Push、Recorder、Cue強化
- v1.0 Node: Scheduler + `--player`、多台同期、RPi公式、72h耐久保証

## 11. リスク・未決

- [ ] NDI SDKのAGPL非両立 -> Plugin分離で解決予定、法務レビュー要
- [ ] RPi性能 (4K HAPは不可、1080p H.264 HWデコードに限定) -> 受信ノード用途に割切るか検証要
- [ ] RTMPはレガシー化 -> SRT中心、RTMPは互換維持のみ。新規はSRT推奨と明示
- [ ] FFmpegのライセンス (gplビルド要否) -> HAP等に必要ならビルドフラグ管理
- [ ] ISFフル互換 or サブセットか -> v0.1はサブセットで開始
- [ ] 寄付導線 (GitHub Sponsors / Open Collective) -> v0.2までに決定

## 12. 次アクション

1. 本SPEC合意 -> repo初期化 (cargo workspace + wgpu hello-triangle + egui + i18n雛形)
2. Stream trait + Quadレンダーのスパイク
3. locales/ja,en + Noto Sans組込
4. CI (macOS/Win/Linuxビルド + clippy + soak)
