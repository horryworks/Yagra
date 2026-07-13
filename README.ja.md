# Yagra

Yagra は、ネットワークデバイスやサーバを **ICMP / SNMP / API コール** で監視する
**ネットワーク監視システム（NMS）** です。死活・性能・閾値を継続的に監視し、異常時に
アラートを発出します。Docker で動作し、**数万ノード規模**と**分散ポーリング**を最初から
見据えたアーキテクチャを採用しています。利用者は WebUI からアクセスします。

> ステータス: **v0.1.11。** ICMP / SNMP v2c+v3 / URL 監視 / Cisco Meraki（読み取り専用 Dashboard
> API）、受動イベント監視、探索・分類、アラート、ダッシュボード、レポートを備えたスタックが、
> PostgreSQL / Redis / NATS / VictoriaMetrics 上で Docker Compose により動作します。既定は単一
> ノードですが、**分散ポーラプール**（拠点に配置したリモートポーラをロケーション親和で割り当て、
> 障害時は自動フェイルオーバー）でスケールアウトできます。各バイナリは **OpenTelemetry トレース**
> （オプトイン）を送出し、System Health ではコアと各ポーラの**ホストリソース傾向**（CPU / ロード /
> メモリ / ディスク）を表示、再起動時にはグレースフルに停止します。WebUI は **英語と日本語**を
> ほとんどの画面でその場で切り替えられ、**SNMPv3** ノードは GETBULK テーブルウォークでインタフェース
> 単位のメトリクスを収集します。HA ストアは書き換えではなく設定で対応する段階に留まります。

## コンポーネント

各バックエンドコンポーネントは `crates/` 配下のワークスペースクレート、WebUI は `web/` 配下です。

| コンポーネント | 役割 | クレート / ディレクトリ |
|---|---|---|
| Yagra-core | オーケストレーション、スケジューリング、北向き API | `crates/yagra-core` |
| Yagra-poller | ICMP/SNMP/API の実ポーリング（ステートレス・水平スケール） | `crates/yagra-poller` |
| Yagra-discovery | デバイス探索・分類 | `crates/yagra-discovery` |
| Yagra-alert | 状態判定・ヒステリシス・依存抑制 | `crates/yagra-alert` |
| Yagra-ingest | 受動イベント解析（syslog / SNMPトラップ）+ レート制限 | `crates/yagra-ingest` |
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

## 受動イベント監視（syslog / SNMPトラップ / Webhook）

能動ポーリングに加えて、Yagra は受信イベントをオペレータ定義のルール（部分一致 / 正規表現）と
照合してアラートを発報し、PagerDuty（Events API v2）や Jira Service Management（Alerts API）へ
fire/resolve ライフサイクル連動で転送できます:

- **syslog**（UDP 514）と **SNMPトラップ**（UDP 162、v1/v2c + inform）はポーラが受信し、
  バス経由で core に転送します。拠点ごとに `YAGRA_SYSLOG_BIND` / `YAGRA_TRAP_BIND` で
  有効化します（`docker-compose.yml` 参照）。**SNMPv3 トラップは未対応です。**
- **Webhook** は core API で受信します: `POST /api/v1/ingest/webhook/<source-id>` +
  ソースごとの Bearer トークン（*Alerts ▸ Event sources* で作成）。
- ルール（*Alerts ▸ Event rules*）では重大度、自動クローズ TTL、任意のクリアパターン
  （例: link-up が link-down を解決）、発報しきい値（M 秒間に N 回）を設定できます。
  受信イベントは *Alerts ▸ Events* で参照できます。

> **デプロイ時の注意:** イベント→ノードの相関はデータグラムの**送信元 IP** を使います。
> Docker のブリッジネットワークで送信元 IP が書き換わる環境では、ポーラを
> `network_mode: host` で実行してください。ホストで既に syslog デーモンが 514 番を使って
> いる場合は公開ポートを変更してください。ポーラ側のレート制限（送信元ごと + 全体）が
> イベントフラッドを抑えます。

## デプロイ

単一ノードのフルスタックを 1 コマンドで起動:

```bash
docker compose up --build   # core + poller + web + PostgreSQL/Redis/NATS/VictoriaMetrics
```

WebUI は **http://localhost:3000**、API は **http://localhost:8080**。初回起動時、core は一度限りの
`admin` パスワードをログに出力します（`docker compose logs core`）。

それ以外 — 本番イメージ、Docker を使わない**ネイティブ**実行、リモート拠点への**分散ポーラ** — は
**[DEPLOYMENT.ja.md](DEPLOYMENT.ja.md)**（English: [DEPLOYMENT.md](DEPLOYMENT.md)）を参照してください。
単一ノード / 分散 × Docker / ネイティブの 4 通りすべてに加え、環境変数の完全リファレンスと
アップグレード/バックアップ手順を扱います。

ローカル開発:

```bash
cargo build && cargo test              # バックエンド（Rust ワークスペース）
cd web && npm install && npm run dev   # フロントエンド（Vite 開発サーバ）
```

## ライセンス

MIT
