# Yagra

Yagra は、ネットワークデバイスやサーバを **ICMP / SNMP / API コール** で監視する
**ネットワーク監視システム（NMS）** です。死活・性能・閾値を継続的に監視し、異常時に
アラートを発出します。Docker で動作し、**数万ノード規模**と**分散ポーリング**を最初から
見据えたアーキテクチャを採用しています。利用者は WebUI からアクセスします。

> ステータス: **開発初期。** テスト済みの中核ロジックと、単一プロセスの「歩く骨格」
> （core → bus → poller → メトリクス → API）が動作します。外部サービス（NATS /
> VictoriaMetrics / PostgreSQL / raw-socket ICMP / SNMP）との実連携は実装中です。

## コンポーネント

各バックエンドコンポーネントは `crates/` 配下のワークスペースクレート、WebUI は `web/` 配下です。

| コンポーネント | 役割 | クレート / ディレクトリ |
|---|---|---|
| Yagra-core | オーケストレーション、スケジューリング、北向き API | `crates/saihai` |
| Yagra-poller | ICMP/SNMP/API の実ポーリング（ステートレス・水平スケール） | `crates/banshu` |
| Yagra-discovery | デバイス探索・分類 | `crates/monomi` |
| Yagra-alert | 状態判定・ヒステリシス・依存抑制 | `crates/noroshi` |
| Yagra-bus | ジョブ配信・ポーラ分散 | `crates/hikyaku` |
| Yagra-transport | ICMP/SNMP/HTTP の抽象化 | `crates/sekisho` |
| Yagra-topology | 依存関係・マップ | `crates/nawabari` |
| Yagra-web | ダッシュボード・可視化 | `web/` |

共有ライブラリ: `crates/yagra-common`（横断的な型）、`crates/yagra-secrets`
（資格情報のエンベロープ暗号）。

> クレートのディレクトリ名は当面元の短縮名（`saihai` / `banshu` …）のままですが、上記の
> 機能名 `Yagra-*` が正本で、クレートのリネームを予定しています。

## 技術スタック

- **バックエンド:** Rust — Tokio / Axum / sqlx（PostgreSQL）。`crates/` 配下の Cargo ワークスペース。
- **フロントエンド:** React + TypeScript + Vite、時系列グラフは uPlot。`web/` 配下。
- **ストア:** PostgreSQL（メタデータ）、Redis（キャッシュ/ロック/ポーラ割当）、VictoriaMetrics（TSDB — メトリクス）。
- **バス:** NATS（core⇄poller）。
- **北向き API:** REST（`/api/v1`）。
- **デプロイ:** Docker / Docker Compose（MVP）→ Kubernetes（スケールアウト / HA）。

## はじめに

```bash
# バックエンド（Rust ワークスペース）
cargo build
cargo test

# フロントエンド（web/）
cd web && npm install && npm run dev

# フルスタック（スケルトン）
docker compose up --build
```

## ライセンス

MIT
