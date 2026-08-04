# Yagra

Yagra は、ネットワークデバイスやサーバを **ICMP / SNMP / API コール** で監視する
**ネットワーク監視システム（NMS）** です。死活・性能・閾値を継続的に監視し、異常時に
アラートを発出します。Docker で動作し、**数万ノード規模**と**分散ポーリング**を最初から
見据えたアーキテクチャを採用しています。利用者は WebUI からアクセスします。

> ステータス: **v0.1.21。** ICMP / SNMP v2c+v3 / URL 監視 / DNS 監視 / Cisco Meraki（読み取り専用 Dashboard
> API）、受動イベント監視、探索・分類、アラート、ダッシュボード、レポートを備えたスタックが、
> PostgreSQL / Redis / NATS / VictoriaMetrics 上で Docker Compose により動作します。既定は単一
> ノードですが、**分散ポーラプール**（拠点に配置したリモートポーラをロケーション親和で割り当て、
> 障害時は自動フェイルオーバー）でスケールアウトできます。リモートポーラは**ネットワーク分断を乗り
> 越え**られるようになりました。分断中もローカルでポーリングを続けて結果をバッファし、回線復旧時に
> その期間の**メトリクスを後追いで補完**します（アラートは「今」から再開し、遡って再発報しません）。
> Yagra は **トラフィックフロー**（NetFlow v5/v9・IPFIX・sFlow）を専用の ClickHouse ストアへ収集し、
> ノードごとの**上位トーカー・ポート・プロトコル・AS 単位の通信**を表示します。オフラインの
> **IP→ASN 補完**で通信相手の AS 名を解決し、受信した **SNMP トラップは OID を人間可読な名前**に
> 解決して組み込みのトラップルールで扱います。
> 各バイナリは **OpenTelemetry トレース**（オプトイン）を送出し、System Health ではコアと各ポーラの
> **ホストリソース傾向**（CPU / ロード / メモリ / ディスク）を表示、再起動時にはグレースフルに停止
> します。WebUI は **英語と日本語**をほとんどの画面でその場で切り替えられ、**SNMPv3** ノードは
> GETBULK テーブルウォークでインタフェース単位のメトリクスを収集します。ローカルアカウントに加えて
> **シングルサインオン（OpenID Connect）**でサインインでき、**コアは高可用**（複数インスタンスを自動
> リーダー選出＋フェイルオーバーで運用、オプトイン）に構成できます。HA 構成では**ユーザーセッションを
> コア間で共有**でき（オプトイン）、フェイルオーバーで再ログインを強いられなくなり、公開したバス上の
> **リモートポーラにはプール単位のバス資格情報**を発行して、侵害されたポーラが到達できる範囲を狭められ
> ます（オプトイン）。**AI アシスタントが Yagra を組み込みの MCP ツール面**（`/mcp`、オプトイン）から
> 照会できるようになりました。多くは読み取り専用の状態・メトリクス・フロー・イベント照会とオンデマンドの
> Troubleshoot 分析で、加えて監査付きの書き込み操作（アラートの確認、メンテナンス枠の開始、即時ポーリング）
> を行えます。API トークンで認証し、機器の設定は変更できません。受信した受動データは**そのまま次へ転送**
> できるようになりました。フィルタ付きのティーで syslog・SNMP トラップ・フローエクスポートを SIEM や
> コレクタへ UDP／TCP／TLS でバイト単位そのまま中継し、あるいは正規化した行を **BigQuery** に流し込みます。
> 機器ごとに 2 つ目のエクスポート先を設定するのではなく、送信元 1 か所で完結します。HA ストアは書き換えでは
> なく設定で対応する段階に留まります。

## コンポーネント

各バックエンドコンポーネントは `crates/` 配下のワークスペースクレート、WebUI は `web/` 配下です。

| コンポーネント | 役割 | クレート / ディレクトリ |
|---|---|---|
| Yagra-core | オーケストレーション、スケジューリング、北向き API | `crates/yagra-core` |
| Yagra-poller | ICMP/SNMP/API の実ポーリング（ステートレス・水平スケール） | `crates/yagra-poller` |
| Yagra-discovery | デバイス探索・分類 | `crates/yagra-discovery` |
| Yagra-alert | アラートのプリミティブ — dwell time・フラップ検出・ディスパッチ | `crates/yagra-alert` |
| Yagra-ingest | 受動イベント解析（syslog / SNMPトラップ）+ フロー復号 + レート制限 | `crates/yagra-ingest` |
| Yagra-forward | 転送フィルタ + ワイヤレンダラ（外部コレクタへのティー） | `crates/yagra-forward` |
| Yagra-bus | ジョブ配信・ポーラ分散 | `crates/yagra-bus` |
| Yagra-transport | ICMP/SNMP/HTTP の抽象化 | `crates/yagra-transport` |
| Yagra-topology | 依存関係・マップ | `crates/yagra-topology` |
| Yagra-secrets | 監視資格情報のエンベロープ暗号 | `crates/yagra-secrets` |
| Yagra-authz | ポーラ単位にスコープした NATS 資格情報（Auth Callout） | `crates/yagra-authz` |
| Yagra-telemetry | 構造化ログ + OpenTelemetry エクスポート | `crates/yagra-telemetry` |
| Yagra-hoststats | 自己観測用のホスト CPU/ロード/メモリ/ディスク採取 | `crates/yagra-hoststats` |
| Yagra-web | ダッシュボード・可視化 | `web/` |

横断的な型は `crates/yagra-common` にあります。

> クレート名が実体より狭いものが 2 つあるので、探しに行く前に知っておくと早いです。アラートの
> **エンジン**（状態機械・依存抑制・メンテナンス期間）は `crates/yagra-core/src/alerts.rs` にあり、
> `yagra-alert` はそれが組み合わせるプリミティブ群です。同様に `yagra-discovery` は識別とレート制御
> だけで、ネットワークスイープは `crates/yagra-poller`、分類器は `crates/yagra-core` にあります。

## 技術スタック

- **バックエンド:** Rust — Tokio / Axum / sqlx（PostgreSQL）。`crates/` 配下の Cargo ワークスペース。
- **フロントエンド:** React + TypeScript + Vite、時系列グラフは uPlot。`web/` 配下。
- **ストア（5 つ）:** PostgreSQL（メタデータと、リーダー選出に使う advisory lock）、
  VictoriaMetrics（TSDB — メトリクス）、Redis（ポーラの生存/割当のミラー — 再構築可能）、
  VictoriaLogs（受動イベントストア、任意）、ClickHouse（トラフィックフローストア、任意）。
  後ろの 2 つはオプトインですが、既定の compose では起動します。
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

## AI クライアントの接続（MCP）

Yagra は**読み取り専用の [MCP](https://modelcontextprotocol.io) ツール面**（ADR-028）を公開でき、
AI クライアント（Claude Code / Claude Desktop / その他 MCP 対応アシスタント）から監視状態を自然言語で
問い合わせられます。例:「落ちているノードは?」「アクティブなアラートを要約して」「edge-router-1 の
直近1時間の CPU を見せて」「アノマリー検知を実行して異常を教えて」。ほとんどのツールは**読み取り専用**で、
AI が見るのは WebUI と同じデータです。同じオンデマンドの**トラブルシュート**分析も起動できます（分析は
メトリクス履歴を読んで所見を返すのみ）。少数の**書き込み**ツールは*監視*システムに作用できます — アラート
の確認応答、メンテナンス枠の作成、即時ポーリングの実行 — ただしロールが許可するトークンのみ（**Viewer**
トークンは読み取り専用）で、書き込みはすべて監査ログに記録されます。**ネットワーク機器を設定・変更する
ツールは依然としてありません**。

読み取りツール: `get_fleet_summary`, `list_nodes`, `get_node_status`, `get_active_alerts`,
`get_alert_history`, `query_metrics`, `get_topology`, `top_flows`, `search_events`（syslog / トラップ /
webhook）、およびトラブルシュート3種 `run_analysis`, `get_analysis_findings`, `list_analyses`
（オンデマンドの anomaly / correlation / capacity / flap 分析）。
書き込みツール（Operator/Admin トークンが必要・全呼び出しを監査）: `ack_alert`, `open_maintenance`,
`poll_now`。

### 1. サーバを有効化

既定 OFF。core に `YAGRA_ENABLE_MCP=true` を設定して（`docker-compose.yml` のコメントを外すか、
deploy compose なら `.env` に追記）再起動します。エンドポイントは **API ポート**の `/mcp` に出ます:

```
https://<yagra-host>/mcp              # WebUI の TLS エッジ経由（推奨）
http://<yagra-host>:8080/mcp          # core の API ポート直（平文）
```

どちらでも動きます。推奨は TLS のほう — web コンテナが `/mcp` を core へプロキシするので暗号化されます。
また、多くのクライアントが受け付けるのはこちらだけです。自己署名の初期証明書のままだと、証明書を信頼させる
設定ができないクライアントは core の平文ポートに退避するしかありません。Settings ▸ TLS で正式な証明書を
取り込むことが、1 つめの URL をどこでも使えるようにする条件です。

⚠️ `YAGRA_MCP_ALLOWED_HOSTS` を設定している場合は web 側のホスト名も列挙してください。これは `Host` ヘッダに
対する照合で、上の 2 つの URL では値が異なります。

無効時は未マウント（404）で従来と byte-identical。MCP は `YAGRA_PUBLIC_DASHBOARD` が ON でも **常に認証必須**です。

### 2. API トークンを発行

WebUI に管理者でサインイン → **Settings ▸ API tokens ▸ New token** →「使用できる面」で **MCP** に
チェック → 読み取り専用アシスタントなら **Viewer**（読み取り／トラブルシュート系は全て使えます）、
アラート確認応答・メンテナンス枠作成・即時ポーリングもさせたいなら **Operator/Admin** を選び、
一度だけ表示される `yat_…` をコピー。これが AI クライアントが送る Bearer トークンです。（通常の
ログインセッショントークンでも動きますが期限切れになります。API トークンは無人クライアント向けで、
同じ画面から失効できます。）

同じ種類のトークンが **REST API** の認証にも使えます。**REST API** にもチェックを入れるか、REST 用に
別のトークンを発行してください。アシスタント用のトークンを MCP だけに留められることが、この項目の
存在理由です。無人利用では、トークンの所有者を**サービスアカウント**（Settings ▸ Users & roles →
アカウント種別で*サービスアカウント*）にしてください。パスワードを持たずサインインできないため、
資格情報が作成者より長生きし、そのアカウント 1 つを無効化すれば所有するトークンがすべて止まります。

> **到達性:** HTTP 呼び出しは Anthropic のクラウドではなく**あなたの手元のクライアント**から出ます。
> よってクライアントが `<yagra-host>:8080` に到達できれば十分です（同一 LAN、または VPN 経由）。
> claude.ai の Web アプリから繋ぐ場合を除き、インターネットへのインバウンド公開は不要です（下記参照）。

### 3. クライアントに登録

**Claude Code（CLI / VS Code 拡張）** — 全プロジェクト・全ディレクトリで使えるよう **`--scope user`** を付けます:

```bash
claude mcp add --scope user --transport http yagra http://<yagra-host>:8080/mcp \
  --header "Authorization: Bearer yat_your_token"
```

`--scope user` を付けないと `claude mcp add` は *local* スコープに入り、**CLI では見えても VS Code 拡張は
local スコープを読み込みません**（拡張は user スコープと project の `.mcp.json` のみ）。そのため拡張の `/mcp`
に出ません。また MCP はセッション開始時に読み込まれるので、追加後は**ウィンドウをリロード / 新規セッション**
してください。その後 `/mcp` で `yagra` が connected として出て、ノード一覧やアラート要約を依頼できます。

**Claude Desktop** — Desktop は `mcp-remote` ヘルパー経由でリモート HTTP サーバに橋渡しします。
`claude_desktop_config.json`（Settings ▸ Developer ▸ Edit config）に以下を追加し、Desktop を再起動:

```json
{
  "mcpServers": {
    "yagra": {
      "command": "npx",
      "args": [
        "-y", "mcp-remote", "http://<yagra-host>:8080/mcp",
        "--header", "Authorization: Bearer yat_your_token"
      ]
    }
  }
}
```

**Gemini CLI** — `~/.gemini/settings.json`（またはプロジェクトの `.gemini/settings.json`）に追加します。
`httpUrl` キーで Streamable HTTP トランスポートが選択されます:

```json
{
  "mcpServers": {
    "yagra": {
      "httpUrl": "http://<yagra-host>:8080/mcp",
      "headers": { "Authorization": "Bearer yat_your_token" }
    }
  }
}
```

`gemini` を再起動して `/mcp` を実行すると Yagra のツールが一覧されます。VS Code の Gemini Code Assist も
同じ `settings.json` を読みます。

**claude.ai（Web）/ Team / Enterprise** — **カスタムコネクタ**（Settings ▸ Connectors）として追加します。
これは `/mcp` が Anthropic のサーバから到達できること、すなわち**公開 HTTPS URL**（リバースプロキシや
Cloudflare Tunnel でフロントする）が前提です。LAN/VPN のみのアドレスでは繋がりません。Gemini の Web アプリ /
Vertex AI エージェントのコネクタから繋ぐ場合も同じく公開 HTTPS が必要です。

**任意の MCP クライアント / `curl` での疎通確認** — トランスポートは HTTP 上の素の JSON-RPC なので、
クライアント無しでスモークテストできます:

```bash
curl -sN http://<yagra-host>:8080/mcp \
  -H "Authorization: Bearer yat_your_token" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

> **注意:** AI は稼働中の監視データ（ノード名・アドレス・アラート）を読み、クラウド型アシスタントでは
> それが会話コンテキストとしてモデルプロバイダに送られます。ツール出力は境界外に出るデータとして扱って
> ください。機器の資格情報はどのツール結果にも**絶対に含まれません**。トークンは最小権限（Viewer）に保ち、
> クライアントが不要になったら Settings ▸ API tokens から失効してください。

## デプロイ

単一ノードのフルスタックを 1 コマンドで起動:

```bash
docker compose up --build   # core + poller + web、および PostgreSQL / Redis / NATS /
                            # VictoriaMetrics / VictoriaLogs / ClickHouse
```

WebUI は **https://localhost:8443**、API は **http://localhost:8080**。初回起動時、core は一度限りの
`admin` パスワードをログに出力します（`docker compose logs core`）。

WebUI は既定で HTTPS です。core が初回起動時に自己署名証明書を生成するのでブラウザが一度警告を出しますが、
**Settings ▸ TLS** で正式な証明書を取り込めば再起動なしで数秒で切り替わります。

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

Copyright (C) 2026 horryworks. Yagra は **GNU Affero General Public License v3.0 only**
（`AGPL-3.0-only`）で提供されます — [LICENSE](LICENSE) と [NOTICE](NOTICE) を参照してください。
Yagra は通常ネットワークサービスとして運用されるため、AGPL の **§13** に注意してください:
**改変版**をネットワーク越しに利用者へ提供する場合、その改変版の対応ソース（Corresponding
Source）を利用者へ提示する義務があります。

**実際に何が求められるか。** AGPL は条文の中身よりも評判で敬遠されがちなので、平易な言葉で整理します
（正文は [LICENSE](LICENSE) であり、以下は要約であって法的助言ではありません）:

- **無改変で運用する分には、何も発生しません。** §13 が適用されるのは*プログラムを改変した場合*だけ
  です。公開イメージをそのままデプロイし、設定し、自社ネットワークを監視する — 利用者が自社の担当者
  でも顧客でも、ソース開示義務は生じません。
- **あなたのデータと設定は対象外です。** §13 が及ぶのは Yagra 自身のソースコードです。ノードインベントリ、
  ダッシュボード、しきい値、アラート履歴、収集したメトリクス、資格情報、そして Yagra に与えた設定は
  あなたのものであり、何も開示されません。
- **「利用者へ提示」は「世界に公開」ではありません。** Yagra を改変してネットワーク越しに提供する場合、
  対応ソースの提示先はそのインスタンスを利用している人々 — 社内デプロイなら自社の担当者、ホスティング
  して提供しているなら顧客 — です。一般公開する義務も、上流へ還元する義務もありません。
- **クライアントを書くことは Yagra の改変ではありません。** Yagra の REST API や MCP に接続する独立した
  プログラムは、通常は別個の著作物として扱われます。ワークスペース*内部*に足したコード — 新しい
  チェック種別、ポーラの変更、WebUI の変更 — は改変にあたります。
- **もう一つの契機は再配布で、これは GPL と同じです。** Yagra のバイナリやイメージを第三者に渡す場合は、
  改変の有無にかかわらず通常の §6 のソース提供義務がかかります。これは GPL-3.0 でも同一で、AGPL 固有の
  条項ではありません。

AGPL 以外の条件での利用（プロプライエタリ製品への組み込み、ソース開示義務なしでの改変版
運用など）については、別途**商用ライセンス**を提供できる場合があります —
horryworks@gmail.com までお問い合わせください。
