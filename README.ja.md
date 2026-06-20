# Yagra

Yagra は、ネットワークデバイスやサーバを **ICMP / SNMP / API コール** で監視する
**ネットワーク監視システム（NMS）** です。死活・性能・閾値を継続的に監視し、異常時に
アラートを発出します。Docker で動作し、**数万ノード規模**と**分散ポーリング**を最初から
見据えたアーキテクチャを採用しています。利用者は WebUI からアクセスします。

> ステータス: **v0.1.0 — 初回リリース。** ICMP / SNMP v2c+v3 / URL 監視、探索・分類、アラート、
> ダッシュボード、レポートを備えた単一ノードスタックが、PostgreSQL / Redis / NATS /
> VictoriaMetrics 上で Docker Compose により動作します。スケールアウト（分散ポーラ、HA ストア）は
> 書き換えではなく設定で対応する設計です。

## コンポーネント

各バックエンドコンポーネントは `crates/` 配下のワークスペースクレート、WebUI は `web/` 配下です。

| コンポーネント | 役割 | クレート / ディレクトリ |
|---|---|---|
| Yagra-core | オーケストレーション、スケジューリング、北向き API | `crates/yagra-core` |
| Yagra-poller | ICMP/SNMP/API の実ポーリング（ステートレス・水平スケール） | `crates/yagra-poller` |
| Yagra-discovery | デバイス探索・分類 | `crates/yagra-discovery` |
| Yagra-alert | 状態判定・ヒステリシス・依存抑制 | `crates/yagra-alert` |
| Yagra-bus | ジョブ配信・ポーラ分散 | `crates/yagra-bus` |
| Yagra-transport | ICMP/SNMP/HTTP の抽象化 | `crates/yagra-transport` |
| Yagra-topology | 依存関係・マップ | `crates/yagra-topology` |
| Yagra-web | ダッシュボード・可視化 | `web/` |

共有ライブラリ: `crates/yagra-common`（横断的な型）、`crates/yagra-secrets`
（資格情報のエンベロープ暗号）。

> クレートのディレクトリ名は上記の機能名 `Yagra-*` と一致しています（例: `crates/yagra-core`）。

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

# フルスタック（単一ノードの Docker Compose）
docker compose up --build
```

## ライセンス

MIT
