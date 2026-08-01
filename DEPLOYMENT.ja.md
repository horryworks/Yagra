# Yagra のデプロイ

本ガイドは Yagra の**使い方ではなくデプロイ方法**を扱います。次の組み合わせを網羅します:

|                     | **Docker Compose**                        | **ネイティブ（Docker なし）** |
|---------------------|-------------------------------------------|------------------------------|
| **単一ノード**       | [A](#a--単一ノード-docker-build) · [B](#b--単一ノード-docker-pull) | [C](#c--単一ノード-native)  |
| **分散ポーラ**       | [D](#d--分散ポーラ-docker)                 | [E](#e--分散ポーラ-native)  |

初めてなら **[A](#a--単一ノード-docker-build)**（1 コマンド）か **[B](#b--単一ノード-docker-pull)**（本番相当・ビルド済みイメージを取得）から。リモート拠点にポーラを置く段階になったら **[D](#d--分散ポーラ-docker)** へ。

English: **[DEPLOYMENT.md](DEPLOYMENT.md)**.

---

## トポロジとバックエンドサービス

Yagra は 2 つの常駐バイナリと静的 WebUI、そして 5 つのストアとバスで構成されます:

- **Yagra-core**（`yagra-core`）— オーケストレーション、スケジューラ、北向き REST API（`/api/v1`）+ Prometheus `/metrics`。**PostgreSQL + NATS + VictoriaMetrics が必須。Redis は任意**（ポーラの死活/割当ミラー。無くても起動を妨げず、機能が縮退するだけ）。必須 3 URL のいずれかが未設定だと、core はライブ動作せずインメモリの**スケルトン**モードに落ちます。
- **Yagra-poller**（`yagra-poller`）— ステートレスな ICMP/SNMP/API ワーカー。**接続先は NATS のみ**。デバイス資格情報・ジョブ仕様・結果はすべてバス経由で流れ、ポーラは PostgreSQL/Redis/VictoriaMetrics に直接触れません。これが水平スケールとリモート配置を可能にしています。
- **Yagra-web** — React/Vite の SPA を静的ファイルにビルドし、nginx が配信して `/api` を core へリバースプロキシします。core とは**別成果物**です（core は API + metrics のみを提供し、静的ファイルは配信しません）。

| ストア / バス | 役割 | 必要とするもの |
|---|---|---|
| **PostgreSQL** | メタデータ: ノード・設定・閾値・ユーザ・アラート履歴 | core（必須） |
| **NATS**（JetStream） | core⇄poller バス: ジョブ・作業セット・結果・イベント | core + poller（必須） |
| **VictoriaMetrics** | TSDB: メトリクス本体ストア | core（必須） |
| **Redis** | 一時状態: ポーラ死活/割当ミラー | core（任意） |
| **VictoriaLogs** | パッシブイベントのログストア: イベント検索を支える | core（任意） |
| **ClickHouse** | トラフィックフローストア: フロー機能を支える | core（任意） |

> **スケールアウトの鉄則:** ポーリングの分散は*書き換え*ではなく*設定変更*です。単一ノードでも分散でも同じイメージを動かし、リモートポーラを足し、WAN バスなら NATS の TLS+auth を有効化するだけです。

### ポート

| ポート（コンテナ/バインド） | ホスト既定 | 環境変数 | 用途 | 公開? |
|---|---|---|---|---|
| `8080` | `8080` | `YAGRA_API_ADDR`（ネイティブ）/ `YAGRA_API_PORT`（compose） | core 北向き API + `/metrics` | する |
| `8080`（web nginx） | `3000` | `YAGRA_WEB_PORT` | WebUI | する |
| `1514/udp` | `514` | `YAGRA_SYSLOG_BIND` / `YAGRA_SYSLOG_PORT` | syslog 受信（poller） | 任意 |
| `1162/udp` | `162` | `YAGRA_TRAP_BIND` / `YAGRA_TRAP_PORT` | SNMP トラップ受信（poller） | 任意 |
| `2055/udp` | `2055` | `YAGRA_FLOW_BIND` / `YAGRA_FLOW_PORT` | NetFlow v5/v9 / IPFIX 受信（poller） | 任意 |
| `6343/udp` | `6343` | `YAGRA_SFLOW_BIND` / `YAGRA_SFLOW_PORT` | sFlow v5 受信（poller） | 任意 |
| `9100` | — | （固定） | poller の Prometheus `/metrics` | ネイティブのみ |
| `4222` | — | `YAGRA_NATS_PORT` | NATS バス | 内部。TLS+auth 時のみ公開（D） |
| `5432` / `6379` / `8428` / `9428` / `8123` | — | — | PostgreSQL / Redis / VictoriaMetrics / VictoriaLogs / ClickHouse | 内部のみ |

> MCP ツールサーフェス（`/mcp`、`YAGRA_ENABLE_MCP` でオプトイン）は API ポート `8080` 上で提供されます — 別ポートは開きません。

> **アウトバウンド（転送）。** Settings ▸ Forwarding は、受信した syslog / SNMP トラップ / フロー
> エクスポートを外部コレクタへ中継したり、**Google BigQuery** へクエリ可能な行としてストリーム
> したりします。送信するのはポーラではなく **core** なので、egress が必要なのは core だけです:
> コレクタの `host:port`（UDP・TCP・TLS）へ、BigQuery 宛先なら **`bigquery.googleapis.com` と
> `oauth2.googleapis.com` への HTTPS**（保存鍵の代わりに Workload Identity を使う場合は GCE メタ
> データサーバ `169.254.169.254` も）。宛先を追加するまで何も送信されません。TLS 宛先はコレクタの
> 証明書をコンテナのシステム信頼ストアで検証します。プライベート CA の場合は宛先に PEM を貼り
> 付けてください — 検証を無効化する方法はありません。
>
> **BigQuery 宛先**はデータセットが既に存在している必要があります — Yagra は*テーブル*（日次
> パーティション+クラスタリング）は作りますが、データセットは決して作りません。データセットの
> リージョンは後から変更できず、データ所在地を黙って選ぶのは誤りだからです。書き込み ID には
> データセットに対する **BigQuery データ編集者** ロールを付与してください。行は正規化・型付け
> され、元のバイト列は意図的に**保存しません** — バイト完全なアーカイブも必要なら中継宛先と
> 併用してください。ストリーミング挿入は Google 側で課金されます。
>
> **バス帯域のコスト。** 転送がデバイスの送信内容をそのまま中継できるよう、ポーラは宛先の有無に
> かかわらず元のバイト列を core へ運びます: パッシブイベントは base64 の `raw` フィールドが付き
> （**`yagra.events` 上で 1.45–1.64 倍**、実機トラフィックでの実測値）、受信した各フローデータ
> グラムは集約ストリーム `yagra.flows` に加えて `yagra.flows.raw` 上をそのまま中継されます。
> フローのコストはエクスポータのデータグラムの詰め方に依存し、幅があります: 密に詰まった
> NetFlow v9 エクスポート（~1400 B・~30 レコード）なら **1000 flows/s あたり ≈370 kbit/s**、
> 小さなデータグラムを頻繁に出すデバイス — 実機の UniFi ゲートウェイで実測 — なら
> **~1.0 Mbit/s** 程度です。**1000 flows/s あたり 0.4–1.0 Mbit/s** を見込み、線形にスケール
> させてください（10 000 なら ≈4–10 Mbit/s）。この運搬は意図的な設計です — キャプチャの
> トグルを設けると転送の忠実性が設定次第になってしまうためです — が、**リモート拠点ポーラ**に
> とっては実際の WAN トラフィックなので、拠点回線はこれを見込んでサイジングしてください。
> 宛先が未設定なら core 側のメッセージあたりコストはゼロです。

---

## A — 単一ノード, Docker（ソースからビルド）<a id="a--単一ノード-docker-build"></a>

開発・オールインワン用の構成。`docker-compose.yml` はイメージをローカルで**ビルド**し（タグ `:dev`）、core・poller・web と 5 ストアすべてを 1 ホストで動かします。

```bash
git clone https://github.com/horryworks/Yagra.git
cd Yagra
docker compose up --build          # 単一ノードのフルスタックをビルドして起動
```

WebUI は **http://localhost:3000**（API は http://localhost:8080）。

**初回ログイン。** `YAGRA_ADMIN_PASSWORD` は既定で未設定のため、core は一度限りのランダムな `admin` パスワードを生成し、ログに**一度だけ**出力します:

```bash
docker compose logs core | grep -i password
```

`admin` でログインして変更してください。自分で指定したい場合は `docker-compose.yml` の `core` サービスの `YAGRA_ADMIN_PASSWORD` をコメント解除します。

**稼働内容。** web はホスト `:3000`、API は `:8080`。poller は syslog を `:514/udp`、SNMP トラップを `:162/udp` で受信。PostgreSQL/Redis/NATS/VictoriaMetrics は Docker 内部ネットワークに留まります。マイグレーションは core 起動時に自動実行され、手動手順はありません。名前付きボリューム `pgdata` / `vmdata` が `docker compose down`/`up` をまたいでデータを保持します。

> この構成は評価・開発には十分です。大切なデータを扱うなら **B**（タグ固定 + 永続 KEK により保存済み資格情報が再起動を越えて維持される）を使ってください。

---

## B — 単一ノード, Docker（ビルド済みイメージを取得）<a id="b--単一ノード-docker-pull"></a>

本番相当の単一ノードデプロイ。`docker-compose.deploy.yml` は GHCR からイメージを**取得**し（ローカルビルドなし）、`.env` で完全にパラメータ化され、保存済み監視資格情報が再デプロイを越えて維持されるよう永続 KEK を書き込む one-shot の `kek-init` を追加します。

```bash
git clone https://github.com/horryworks/Yagra.git
cd Yagra
cp .env.example .env                # その後 .env を編集（下記参照）

YAGRA_IMAGE_TAG=latest docker compose -f docker-compose.deploy.yml pull
YAGRA_IMAGE_TAG=latest docker compose -f docker-compose.deploy.yml up -d
```

`YAGRA_IMAGE_TAG` はイメージタグを選びます: `latest` は `main` に追従、`v<version>` タグは安定リリース、不変の `<git-sha>` は特定ビルドを固定（ロールバック = 古い SHA で再実行）。

**`.env` の設定**（`.env.example` からコピー）。要点:

```ini
POSTGRES_PASSWORD=change-me            # 使い捨てでないマシンでは必ず変更
YAGRA_API_PORT=8080                    # API のホストポート
YAGRA_WEB_PORT=3000                    # WebUI のホストポート
# YAGRA_ADMIN_PASSWORD=choose-a-strong-password   # 未設定なら一度限りのランダム値をログ出力
# YAGRA_PUBLIC_DASHBOARD=false         # true = ログイン不要の読み取り専用ダッシュボード
```

**資格情報の永続化（重要）。** `kek-init` サービスは 32 バイトの KEK を `kekdata` ボリュームへ一度だけ書き込み、以後は上書きしません。core はそれを `YAGRA_KEK_FILE=/kek/key` に読み取り専用でマウントします。永続 KEK が無いと core は再起動のたびに再生成される**一時**鍵にフォールバックし、保存済み資格情報（SNMP コミュニティ、API トークン）が再デプロイ後に復号できなくなります。compose がこれを配線済みなので、`kekdata` ボリュームを削除しないでください。

**アップグレード。** 新しいタグを取得して再度 `up -d`:

```bash
YAGRA_IMAGE_TAG=v0.1.4 docker compose -f docker-compose.deploy.yml pull
YAGRA_IMAGE_TAG=v0.1.4 docker compose -f docker-compose.deploy.yml up -d
```

マイグレーションは expand-contract 方式で自動実行され、`pgdata`/`vmdata`/`kekdata` は保持されます。[アップグレードとバックアップ](#アップグレードとバックアップ)を参照。

---

## C — 単一ノード, ネイティブ（Docker なし）<a id="c--単一ノード-native"></a>

バイナリを直接動かす構成。ストアは自分で用意し、ワークスペースをビルドして `yagra-core` + `yagra-poller` を（例えば systemd の）サービスとして動かします。

### 1. バックエンドストアの用意

core を動かすホストから到達可能な状態でインストール・起動します:

- **PostgreSQL 17** — データベースとロールを作成（core が自分でマイグレーションを実行します。データベース自体の作成は**しません**）:
  ```sql
  CREATE ROLE yagra LOGIN PASSWORD 'yagra';
  CREATE DATABASE yagra OWNER yagra;
  ```
- **NATS 2.x（JetStream 有効）** — `nats-server -js`
- **VictoriaMetrics** — `victoria-metrics-prod --retentionPeriod=12`（12 か月、単一ティア）
- **Redis 7**（任意）— ポーラの死活/割当ミラーを有効化するだけ

### 2. ワークスペースのビルド

**Rust 1.90** と（WebUI 用に）**Node 22** が必要です。ベンダリングした `snmp2` パッチ（`Cargo.toml` の `[patch.crates-io]`）を効かせるため、リポジトリのルートからビルドします:

```bash
cargo build --release --workspace           # → target/release/yagra-core, target/release/yagra-poller
cd web && npm ci && npm run build           # → web/dist/  （静的 SPA バンドル）
```

### 3. KEK の用意（core の初回起動前に）

エンベロープ暗号のマスタ鍵 — 永続的な 32 バイトのファイル。これが無いと core は**一時的な dev 鍵**で起動し、資格情報が再起動を越えて維持されません。

```bash
head -c 32 /dev/urandom > /etc/yagra/kek && chmod 0400 /etc/yagra/kek
```

### 4. core の起動

```bash
export YAGRA_DATABASE_URL="postgres://yagra:yagra@localhost:5432/yagra"
export YAGRA_BUS_URL="nats://localhost:4222"
export YAGRA_TSDB_URL="http://localhost:8428"
export YAGRA_REDIS_URL="redis://localhost:6379"     # 任意
export YAGRA_LOGS_URL="http://localhost:9428"       # 任意（未設定ならイベントは PostgreSQL のみ）
export YAGRA_KEK_FILE="/etc/yagra/kek"
export YAGRA_API_ADDR="0.0.0.0:8080"                # 既定値
# export YAGRA_ADMIN_PASSWORD="choose-a-strong-password"   # 未設定なら一度限りのランダム値をログ出力
export RUST_LOG=info

./target/release/yagra-core
```

起動時に core はストアへ接続し、**組み込みマイグレーションを自動実行**、組み込みプロファイル/カタログをシード、`YAGRA_API_ADDR` で `/api/v1` + `/metrics` を提供します。`YAGRA_ADMIN_PASSWORD` が未設定なら、ログから一度限りの `admin` パスワードを拾ってください。

### 5. WebUI の配信

`web/dist/` は静的バンドルです。任意の Web サーバで配信し、`/api` を core へリバースプロキシします。同梱の nginx 設定（`web/nginx.conf`）に倣ってください: SSE のため `proxy_buffering off` と長い `proxy_read_timeout` が必要で、SPA には `try_files … /index.html` フォールバックが必要です。プロキシ先は `http://<core-host>:8080`。

### 6. poller の起動

poller は ICMP のため raw ソケットを必要とします。バイナリに capability を付与して（非 root で動かせます）か、root で実行します:

```bash
sudo setcap cap_net_raw+ep ./target/release/yagra-poller

export YAGRA_BUS_URL="nats://localhost:4222"
export YAGRA_POLLER_ID="poller-1"          # ポーラごとに一意。既定はホスト名
export YAGRA_POLLER_POOL="default"
# 任意の受動イベントリスナ（:514/:162 は root か CAP_NET_BIND_SERVICE が必要）:
# export YAGRA_SYSLOG_BIND="0.0.0.0:1514"
# export YAGRA_TRAP_BIND="0.0.0.0:1162"
export RUST_LOG=info

./target/release/yagra-poller
```

poller は自身の Prometheus `/metrics` を `0.0.0.0:9100` で提供します。

> **任意: PDF レポート。** レポート → PDF エクスポートは `wkhtmltopdf`（パッチ済み Qt ビルド）を呼び出します。未インストールなら PDF エクスポートは HTTP 503（`pdf_unavailable`）を返し、HTML/CSV エクスポートは引き続き動作します。

---

## D — 分散ポーラ, Docker<a id="d--分散ポーラ-docker"></a>

フルスタックを中央で（**B** のように）動かし、リモート拠点にポーラを追加します。各ポーラは拠点のデバイスをローカルにポーリングし、結果をバス経由で返します。ノードは `pool` 属性を持ち、core のコーディネータが各プールのノードをコンシステントハッシュで生存ポーラへ割り当て、障害時は自動フェイルオーバーします。

> **バスはデバイス資格情報を平文で運びます。** 単一ホストなら問題ありません（内部 Docker ネットワーク、何も公開しない）。バスがリモート拠点へ信頼境界を越える瞬間、**まず** TLS 暗号化と認証が必須になります。`:4222` を平文で公開しては**いけません**。

### ステップ 1 — 中央スタックで NATS の TLS + auth を有効化

これは `docker-compose.deploy.yml` の `nats` サービスに（コメントで）既に用意されているオプトインブロックです。5 手順すべてが必須です:

**1a. サーバ証明書を生成**して `./certs` に置きます。SAN には各ポーラがダイヤルする正確なホスト/IP を**必ず**含めます:

```bash
mkdir -p certs
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout certs/server-key.pem -out certs/server-cert.pem \
  -subj "/CN=yagra-nats" \
  -addext "subjectAltName=DNS:nats,DNS:core.example.com,IP:192.168.1.2"
```

証明書は自己署名なので、それ自身が CA です。ステップ 5 で `server-cert.pem`（**公開証明書のみ。鍵は絶対に渡さない**）を配布します。

**1b. バスのパスワードを設定**（意図的に既定値なし）を `.env` に:

```ini
YAGRA_NATS_CORE_PASSWORD=a-strong-core-bus-password
YAGRA_NATS_POLLER_PASSWORD=a-strong-poller-bus-password
YAGRA_NATS_PORT=4222        # バスを公開するホストポート
YAGRA_CERT_DIR=./certs
```

**1c. auth/TLS 設定を読み込む。** `docker-compose.deploy.yml` の `nats` サービスで `command: ["-js"]` をコメントアウトし、その下のブロックをコメント解除します（`command: ["-js", "-c", "/etc/nats/nats-server.conf"]` を設定し、2 つのパスワードを注入し、`docker/nats/nats-server.conf` + `./certs` をマウントし、`${YAGRA_NATS_PORT:-4222}:4222` を公開します）。

**1d. 内部クライアントも TLS に切り替える** — サーバ全体 TLS では平文ポートが残らないため、同居する core と poller も `tls://` を使う必要があります。`core` には:
```yaml
YAGRA_BUS_URL: tls://core:${YAGRA_NATS_CORE_PASSWORD}@nats:4222
```
ローカルの `poller` には:
```yaml
YAGRA_BUS_URL: tls://poller:${YAGRA_NATS_POLLER_PASSWORD}@nats:4222
```
そして**両方**に `YAGRA_BUS_CA_FILE: /etc/nats/certs/server-cert.pem` と `- ${YAGRA_CERT_DIR:-./certs}:/etc/nats/certs:ro` ボリュームマウントを追加します。

**1e.** `certs/server-cert.pem`（公開証明書のみ）を各リモートポーラの運用者に渡します — これが相手側の `YAGRA_BUS_CA_FILE` になります。

中央スタックを起動し直します（`docker compose -f docker-compose.deploy.yml up -d`）。

`nats-server.conf` は `core` にフルアクセスを、`poller` アカウントには最小権限（publish は結果/イベント/heartbeat のみ、subscribe は自分のジョブ + 作業セット割当のみ）を与えます。静的アカウントの制限に注意: **`poller` アカウントは 1 つを共有**するため、認証済みのどのポーラも任意プールの割当を読めます — テナント境界ではありません。バス資格情報をポーラ単位にスコープするには、オプションの **NATS Auth Callout** 連携（`docker/nats/nats-server.conf` の `auth_callout` ブロックと、`.env.example` の `YAGRA_NATS_CALLOUT_*` / `YAGRA_CALLOUT_SEED_DIR` 変数）を有効化してください: core が接続してくる各ポーラに、そのプールのサブジェクトだけにスコープした資格情報を発行するようになります。

### ステップ 2 — WebUI でポーラを登録

**Settings ▸ Pollers ▸ "Register poller"** を開きます。リモートホスト用のすぐ使える `.env`（id / pool / bus URL）を生成します。このポーラに担当させたいプールを割り当てます。

### ステップ 3 — リモートポーラの起動

リモート拠点のマシンで、`docker-compose.poller.yml`（ポーラ**のみ**を動かす）を使います:

```bash
# 生成された .env を docker-compose.poller.yml の隣に置き、CA 証明書を ./certs へ
mkdir -p certs && cp /path/to/server-cert.pem certs/

docker compose -f docker-compose.poller.yml up -d
```

必須の 3 変数（未設定だと compose がエラー終了）— 生成された `.env` が提供します:

```ini
YAGRA_BUS_URL=tls://poller:a-strong-poller-bus-password@core.example.com:4222
YAGRA_POLLER_ID=edge-tokyo-1        # 安定・ポーラごとに一意
YAGRA_POLLER_POOL=tokyo             # このポーラが担当するプール
YAGRA_BUS_CA_FILE=/etc/yagra/certs/server-cert.pem
```

`docker-compose.poller.yml` は `network_mode: host` を使い（受動 syslog/trap の相関が実際のデータグラム送信元 IP を見え、raw ICMP がホストのインターフェースに届く）、`NET_RAW` を付与します。

> **特権ポートの注意。** リモートポーラは**非 root**（ファイル capability `NET_RAW` のみ）で動くため、`:514`/`:162`（< 1024）をバインドできません。既定の高ポート（`1514`/`1162`）を使ってホストのファイアウォールでリダイレクト（`iptables … REDIRECT 514→1514`）するか、デバイスを直接高ポートへ向けてください。起動後数秒で Pollers ページに現れ、core がそのプールのノードを割り当て始めます。

プールをスケールするには、同じ `YAGRA_POLLER_POOL`（かつ別々の `YAGRA_POLLER_ID`）でポーラを増やします — core がプールをそれらへ再分散し、喪失時はフェイルオーバーします。生存ポーラが 0 のプールはレガシーの per-job publish にフォールバックするため、ロールアウト中もノードが止まりません。

---

## E — 分散ポーラ, ネイティブ<a id="e--分散ポーラ-native"></a>

**D** と同じですが、リモートポーラをコンテナではなくネイティブバイナリで動かします。中央バスの TLS+auth 設定（D · ステップ 1）は変わりません。

リモートホストで `yagra-poller` バイナリをビルド（またはコピー）し、CA 証明書を読み取り可能な場所に置いて実行します:

```bash
sudo setcap cap_net_raw+ep ./yagra-poller

export YAGRA_BUS_URL="tls://poller:a-strong-poller-bus-password@core.example.com:4222"
export YAGRA_POLLER_ID="edge-tokyo-1"       # ポーラごとに一意
export YAGRA_POLLER_POOL="tokyo"
export YAGRA_BUS_CA_FILE="/etc/yagra/certs/server-cert.pem"
# 任意の受動イベントリスナ（:514/:162 は root / CAP_NET_BIND_SERVICE が必要）:
# export YAGRA_SYSLOG_BIND="0.0.0.0:1514"
# export YAGRA_TRAP_BIND="0.0.0.0:1162"
export RUST_LOG=info

./yagra-poller
```

受動イベントの送信元 IP 相関と raw ICMP が拠点のインターフェースに対して機能するよう、専用ネームスペースではなくホストネットワーク上で動かしてください。それ以外（プール、登録、フェイルオーバー）は **D** とまったく同じです。

---

## 環境変数リファレンス

### Yagra-core

| 変数 | 既定 | 用途 |
|---|---|---|
| **ストアとバス** | | |
| `YAGRA_DATABASE_URL` | —（ライブに必須） | PostgreSQL 接続文字列 |
| `YAGRA_BUS_URL` | —（ライブに必須） | NATS バス URL（`nats://…` または `tls://user:pass@host:4222`） |
| `YAGRA_TSDB_URL` | —（ライブに必須） | VictoriaMetrics ベース URL |
| `YAGRA_REDIS_URL` | 未設定 ⇒ 無効 | ポーラ死活/割当ミラー用の Redis URL（best-effort） |
| `YAGRA_LOGS_URL` | 未設定 ⇒ イベントは PostgreSQL のみ | VictoriaLogs ベース URL — オプトインのパッシブイベントログストア |
| `YAGRA_CLICKHOUSE_URL` | 未設定 ⇒ フローストア無効 | ClickHouse HTTP URL — オプトインのトラフィックフローストア（未設定だとフロー API は 503 を返す） |
| `YAGRA_PG_MAX_CONNECTIONS` | `20` | PostgreSQL 接続プールの上限（HA のリーダーはこれに加えて advisory lock 用の +1 接続を保持） |
| **API とセキュリティ** | | |
| `YAGRA_KEK_FILE` | 未設定 ⇒ 一時 dev 鍵 | マウントした 32 バイト鍵ファイルへのパス |
| `YAGRA_API_ADDR` | `0.0.0.0:8080` | API + `/metrics` のバインドアドレス |
| `YAGRA_ADMIN_PASSWORD` | 未設定 ⇒ 一度限りのランダム値（ログ出力） | ブートストラップ `admin` パスワード（初回起動のみ） |
| `YAGRA_PUBLIC_DASHBOARD` | `false` | `true` = ログイン不要の読み取り専用ダッシュボード |
| **ポーリングと通知** | | |
| `YAGRA_POLL_INTERVAL_SECS` | `30`（10–3600 にクランプ） | 初期の既定ポーリング間隔（初回起動でシード。以後は DB が権威） |
| `YAGRA_SNMP_COMMUNITY` | 未設定 | 資格情報が未バインドのノードに使うフォールバック SNMP v2c コミュニティ |
| `YAGRA_MERAKI_POOL` | `default` | Meraki クラウド収集ジョブを振り分けるポーラプール |
| `YAGRA_WEBHOOK_URL` | 未設定 ⇒ 無効 | 既定のアラート Webhook チャネル |
| `YAGRA_SMTP_HOST` / `_PORT` / `_FROM` / `_TO` / `_USER` / `_PASS` | 未設定 ⇒ メール無効 | 環境変数による SMTP アラートチャネル（host があれば有効） |
| **トラフィックフローと IP→ASN 補完** | | |
| `YAGRA_FLOW_RETENTION_DAYS` | `30`（1–3650 にクランプ） | フローの保持期間（日数。ClickHouse の TTL） |
| `YAGRA_IPASN_DB` | 未設定 ⇒ 補完無効 | フローの IP→ASN 補完に使うオフライン iptoasn.com TSV へのパス |
| `YAGRA_IPASN_RELOAD_SECS` | `0` ⇒ 起動時に一度だけ読み込み | IP→ASN ファイルのホットリロード周期（秒）。`>0` で再起動なしに再読み込み |
| **高可用性（HA）** | | |
| `YAGRA_ENABLE_HA` | `false` | PostgreSQL advisory lock によるオプトインのアクティブ/パッシブ リーダー選出 |
| `YAGRA_CORE_ID` | 未設定 | HA ログに出すこの core インスタンスの人間可読な識別子 |
| `YAGRA_SESSION_KEY_FILE` | 未設定 ⇒ プロセス内トークン | マウントした HMAC セッション署名鍵へのパス（セッションがどの core でも・再起動をまたいでも有効になる）。設定済みで読めない/不正なら起動失敗 |
| **MCP（AI クライアント）** | | |
| `YAGRA_ENABLE_MCP` | `false` | API ポート上の `/mcp` に MCP ツールサーフェスをマウント（認証は常に必須） |
| `YAGRA_MCP_ALLOWED_HOSTS` | 未設定 ⇒ 任意の `Host` を受理 | `/mcp` の `Host` ヘッダ許可リスト（カンマ区切り。DNS リバインディング対策） |
| **分析と RCA のレート上限** | | |
| `YAGRA_ANALYSIS_MAX_CONCURRENT` | `4` | 同時実行できるトラブルシュート分析の上限 |
| `YAGRA_ANALYSIS_RATE_PER_MIN` | `30` | 毎分受け付ける新規分析の上限 |
| `YAGRA_RCA_MAX_CONCURRENT` | `2` | 同時実行できる LLM 根本原因分析の上限（課金される外部呼び出し） |
| `YAGRA_RCA_RATE_PER_MIN` | `10` | 毎分受け付ける新規根本原因分析の上限 |
| `YAGRA_RCA_CACHE_SECS` | `900` | RCA レポートのキャッシュ寿命（秒）。`force` はキャッシュを迂回するが上限は迂回しない |
| **NATS Auth Callout（ポーラごとのバス資格情報）** | | |
| `YAGRA_NATS_CALLOUT_SEED_FILE` | 未設定 ⇒ callout 無効 | マウントした NATS アカウント nkey シードへのパス。設定すると core がポーラごとにスコープしたバスユーザを発行 |
| `YAGRA_NATS_CALLOUT_ACCOUNT` | `$G` | 発行したポーラユーザを配置する NATS アカウント（サーバの `auth_callout` アカウントと一致必須） |
| `YAGRA_NATS_POLLER_PASSWORD` | 未設定 ⇒ callout 無効 | callout が検証するポーラ共有のブートストラップシークレット（NATS サーバ設定も消費） |
| **可観測性** | | |
| `YAGRA_DISK_WATCH_PATHS` | `/=root` | ホスト自己メトリクスが容量を報告するファイルシステム（カンマ区切りの `path` または `path=alias`）。core と poller の**両方**が読む |
| `YAGRA_OTEL_ENDPOINT` | 未設定 ⇒ ログのみ | OpenTelemetry トレース送出先の OTLP/HTTP エンドポイント（`OTEL_EXPORTER_OTLP_ENDPOINT` にフォールバック） |
| `OTEL_TRACES_SAMPLER` / `_ARG` | `parentbased_always_on` | トレースサンプラ。大規模時は `parentbased_traceidratio` + 引数（例 `0.01`）を使用 |
| `RUST_LOG` | `info` | ログレベル（例 `info,yagra_core=debug`） |

### Yagra-poller

| 変数 | 既定 | 用途 |
|---|---|---|
| **識別子とバス** | | |
| `YAGRA_BUS_URL` | 未設定 ⇒ アイドル | NATS バス URL（ポーラが張る唯一のバックエンド接続） |
| `YAGRA_POLLER_ID` | ホスト名、無ければ `poller-<hex>` | 安定・一意・subject 安全なポーラ識別子 |
| `YAGRA_POLLER_POOL` | `default` | このポーラが担当するプール |
| `YAGRA_BUS_CA_FILE` | 未設定 ⇒ 平文 | `tls://` バスに固定する CA/サーバ証明書 |
| `YAGRA_MAX_CONCURRENT_POLLS` | `64` | 同時実行プローブ数の上限 |
| `YAGRA_POLLER_QUEUE` | `pollers` | 負荷分散ジョブ消費用の NATS キューグループ |
| **パッシブイベント（syslog / SNMP トラップ）** | | |
| `YAGRA_SYSLOG_BIND` | 未設定 ⇒ 無効 | syslog 受信の UDP バインド（例 `0.0.0.0:1514`） |
| `YAGRA_TRAP_BIND` | 未設定 ⇒ 無効 | SNMP トラップ受信の UDP バインド（v1/v2c） |
| `YAGRA_TRAP_COMMUNITY` | 未設定 ⇒ フィルタなし | コミュニティ不一致のトラップを破棄（値はログしない） |
| `YAGRA_EVENT_RATE_PER_SOURCE` | `200` | 送信元 IP ごとのパッシブイベントレート制限（件/秒） |
| `YAGRA_EVENT_RATE_GLOBAL` | `5000` | 全送信元合計のパッシブイベントレート制限（件/秒） |
| **トラフィックフロー（NetFlow / IPFIX / sFlow）** | | |
| `YAGRA_FLOW_BIND` | 未設定 ⇒ 無効 | NetFlow v5/v9 / IPFIX 受信の UDP バインド（例 `0.0.0.0:2055`） |
| `YAGRA_SFLOW_BIND` | 未設定 ⇒ 無効 | sFlow v5 受信の UDP バインド（例 `0.0.0.0:6343`） |
| `YAGRA_FLOW_RATE_PER_SOURCE` | `1000` | エクスポータごとのフローレート制限（データグラム/秒。syslog/トラップとは別枠） |
| `YAGRA_FLOW_RATE_GLOBAL` | `20000` | 全エクスポータ合計のフローレート制限（データグラム/秒） |
| `YAGRA_FLOW_BUCKET_SECS` | `60` | フロー集約バケット幅（秒） |
| `YAGRA_FLOW_TOP_N` | `500` | バケット×エクスポータごとに保持する上位フロー数（バイト順）— カーディナリティ制御の要 |
| **エッジリスナのチューニング** | | |
| `YAGRA_LISTENER_WORKERS` | CPU 数（1–4 にクランプ） | UDP リスナごとの並列読み取りソケット数（Linux の `SO_REUSEPORT`。他 OS は 1 ソケット） |
| `YAGRA_LISTENER_RCVBUF_BYTES` | `4194304`（4 MiB） | リスナソケットごとの受信バッファサイズ |
| **store-and-forward 結果バッファ** | | |
| `YAGRA_STORE_FORWARD` | 有効（`off`/`false`/`0`/`no` で無効） | バス断の間ポーリング結果をバッファし、復旧後に再送 |
| `YAGRA_STORE_FORWARD_DIR` | `/var/lib/yagra/buffer` | ディスクへの退避ディレクトリ（書き込めなければメモリのみに縮退） |
| `YAGRA_STORE_FORWARD_MEM_MAX` | `20000` | ディスク退避を始めるまでのメモリリングサイズ（結果件数） |
| `YAGRA_STORE_FORWARD_DISK_MAX_MB` | `512` | ディスク退避の合計上限（古いセグメントから破棄） |
| `YAGRA_STORE_FORWARD_DISK_FREE_FLOOR_MB` | `1024` | ファイルシステムの空きがこれを下回ったら退避を停止 |
| `YAGRA_STORE_FORWARD_MAX_AGE_SECS` | `86400` | これより古いバッファ済み結果は再送時に破棄 |
| `YAGRA_STORE_FORWARD_SEGMENT_MB` | `16` | 退避セグメントのロールサイズ（ディスク上限の粒度） |
| **可観測性** | | |
| `YAGRA_OTEL_ENDPOINT` | 未設定 ⇒ ログのみ | トレース送出先の OTLP/HTTP エンドポイント（core と同じコレクタ） |
| `OTEL_TRACES_SAMPLER` / `_ARG` | `parentbased_always_on` | トレースサンプラ。大規模時はサンプリング（`parentbased_traceidratio`） |
| `RUST_LOG` | `info` | ログレベル |

> **compose 専用の変数**は Docker Compose / NATS 設定が消費するもので、Rust バイナリは読みません — バイナリが見るのは最終的に組み立てられた `YAGRA_BUS_URL` などだけです。`.env.example` を参照:
>
> - イメージとストア: `YAGRA_IMAGE_TAG`, `POSTGRES_PASSWORD`
> - ホストポートのマッピング: `YAGRA_API_PORT`, `YAGRA_WEB_PORT`, `YAGRA_SYSLOG_PORT`, `YAGRA_TRAP_PORT`, `YAGRA_FLOW_PORT`, `YAGRA_SFLOW_PORT`, `YAGRA_NATS_PORT`
> - バスの TLS + auth（D）: `YAGRA_CERT_DIR`, `YAGRA_NATS_CORE_PASSWORD`, `YAGRA_NATS_POLLER_PASSWORD`（core も Auth Callout のブートストラップシークレットとして読む）, `YAGRA_NATS_CALLOUT_ISSUER`（NATS サーバが core の callout JWT を検証するアカウント公開鍵）
> - マウントする鍵ディレクトリ: `YAGRA_SESSION_KEY_DIR`（`YAGRA_SESSION_KEY_FILE` 用の `session.key` を置く）, `YAGRA_CALLOUT_SEED_DIR`（`YAGRA_NATS_CALLOUT_SEED_FILE` 用の `account.seed` を置く）
> - IP→ASN 更新サイドカー: `YAGRA_IPASN_URL`（データセット URL）, `YAGRA_IPASN_REFRESH_SECS`（取得周期。既定 `604800` = 週次）

---

## 分散トレーシング（OpenTelemetry）<a id="分散トレーシング"></a>

各バイナリは構造化ログと Prometheus `/metrics` を標準で出力します。**分散トレーシングはオプトイン**です: `YAGRA_OTEL_ENDPOINT`（または標準の `OTEL_EXPORTER_OTLP_ENDPOINT`）に OTLP/HTTP コレクタを設定すると、core と poller が 1 回のポーリングをエンドツーエンドで繋ぐ span（core の dispatch → poller の poll → core の ingest）＋北向き API リクエストごとの span を送出します。未設定なら**トレーシングのオーバーヘッドはゼロ**（ログのみ）で、単一構成 MVP はコレクタ不要です。

- **ローカルで試す:** `docker compose --profile tracing up` で同梱の Jaeger（UI は http://localhost:16686）が起動します。次に `docker-compose.yml` の `core` と `poller` の**両方**で `YAGRA_OTEL_ENDPOINT: http://jaeger:4318` をコメント解除します。
- **大規模時はサンプリング。** 数万ノードが間隔ごとにポーリングすると 1 ポーリング＝1 トレースになります。`OTEL_TRACES_SAMPLER=parentbased_traceidratio` と `OTEL_TRACES_SAMPLER_ARG=0.01`（1%）を設定してください。`parentbased_*` は core⇄poller をまたいでトレース全体の判定を一貫させます。トレースコンテキストはバス上の `trace_context` フィールドで運ばれ、**トレーシング無効時は wire に出ず**、N-1 ピアは無視します（N/N-1 安全）。
- **本番:** エンドポイントは、バックエンド（Tempo, Jaeger, Honeycomb など）へ転送する OpenTelemetry Collector に向けます。リモート拠点のポーラは、NATS バスとは別に、到達可能な独自のコレクタエンドポイントが必要です。

---

## アップグレードとバックアップ<a id="アップグレードとバックアップ"></a>

アップグレードは低コストで、データを**決して**失わず・壊さないよう設計されています:

- **DB マイグレーションは expand-contract で、core 起動時に自動実行**されます。N→N+1 は常にサポートされ、手動マイグレーション CLI はありません。
- **バスはバージョン耐性（N/N-1）があります。** 新しい core は古いポーラとロールアウト中も動作するため、core を先に、ポーラを後にアップグレードできます。
- **ローリングアップグレード。** ポーラはステートレスなので任意の順で入れ替え可能です。Docker なら新タグを取得して `up -d`（**B** 参照）。リモートポーラは拠点ごとに取得して `up -d`。一時的に落ちたプールはレガシー publish にフォールバックするため、ノードは止まりません。
- **大きなアップグレードの前に永続ストアをバックアップ:** `pgdata`（PostgreSQL）、`vmdata`（VictoriaMetrics）、`kekdata`（KEK）ボリューム — またはネイティブ相当。**Redis は再構築可能**なので、失っても致命的ではありません。

> **KEK を失わないこと。** `kekdata` ボリューム / KEK ファイルが壊れると、保存済みの監視資格情報はすべて永久に復号不能になります。データベースと一緒にバックアップしてください。

---

## セキュリティ上の注意

- **信頼境界を越えるバスでは TLS が必須です。** ジョブメッセージはデバイス資格情報を平文で運びます — NATS `:4222` を平文でリモート拠点へ公開しないでください（**D · ステップ 1** 参照）。
- **KEK はマウントしたファイルであり、環境変数の値ではありません。** `YAGRA_KEK_FILE` で渡します。一時鍵フォールバックは開発専用です。
- **イメージは非 root で動きます**（core uid 10001、poller uid 10002 + `cap_net_raw+ep` ファイル capability、web nginx uid 101）。`NET_RAW` を得るのは poller だけです。
- **資格情報はログしません** — SNMP コミュニティ、SNMPv3 auth/priv、API トークンは保存時に暗号化され、ログ・API 応答・メトリクスラベルから伏せられます。
