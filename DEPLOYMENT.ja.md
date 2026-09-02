# Yagra のデプロイ

本ガイドは Yagra の**使い方ではなくデプロイ方法**を扱います。次の組み合わせを網羅します:

|                     | **Docker Compose**                        | **ネイティブ（Docker なし）** |
|---------------------|-------------------------------------------|------------------------------|
| **単一ノード**       | [A — ビルド済みイメージ](#a--単一ノード-docker-pull) · [B — ソースからビルド](#b--単一ノード-docker-build) | [C](#c--単一ノード-native)  |
| **分散ポーラ**       | [D](#d--分散ポーラ-docker)                 | [E](#e--分散ポーラ-native)  |

**まずは [A](#a--単一ノード-docker-pull)** から。公開イメージを取得するだけでチェックアウトもビルドも要らず、単一ノード構成のなかで唯一 WebUI から自分自身をアップグレードできます。リモート拠点にポーラを置く段階になったら **[D](#d--分散ポーラ-docker)** へ。

残る 3 つは対象が限られます。**[B](#b--単一ノード-docker-build)** はソースからビルドする構成で、Yagra を開発する・監査する・独自ビルドを作るための経路です。**[C](#c--単一ノード-native)** と **[E](#e--分散ポーラ-native)** は Docker が使えないホスト向けにバイナリを直接動かします。いずれもサポート対象で本ガイドで扱いますが、監視システムを立ち上げたいのであれば A を選んでください。

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
| `8080` | `8080` | `YAGRA_API_ADDR`（ネイティブ）/ `YAGRA_API_PORT` + `YAGRA_API_BIND`（compose） | core 北向き API + `/metrics` — **平文** | する |
| `8080`（web nginx） | **`443`** | `YAGRA_WEB_PORT` | WebUI — **HTTPS**（`YAGRA_WEB_TLS`） | する |
| `1514/udp` | `514` | `YAGRA_SYSLOG_BIND` / `YAGRA_SYSLOG_PORT` | syslog 受信（poller） | 任意 |
| `1162/udp` | `162` | `YAGRA_TRAP_BIND` / `YAGRA_TRAP_PORT` | SNMP トラップ受信（poller） | 任意 |
| `2055/udp` | `2055` | `YAGRA_FLOW_BIND` / `YAGRA_FLOW_PORT` | NetFlow v5/v9 / IPFIX 受信（poller） | 任意 |
| `6343/udp` | `6343` | `YAGRA_SFLOW_BIND` / `YAGRA_SFLOW_PORT` | sFlow v5 受信（poller） | 任意 |
| `9100` | — | （固定） | poller の Prometheus `/metrics` | ネイティブのみ |
| `4222` | — | `YAGRA_NATS_PORT` | NATS バス | 内部。TLS+auth 時のみ公開（D） |
| `5432` / `6379` / `8428` / `9428` / `8123` | — | — | PostgreSQL / Redis / VictoriaMetrics / VictoriaLogs / ClickHouse | 内部のみ |

> MCP ツールサーフェス（`/mcp`、`YAGRA_ENABLE_MCP` でオプトイン）は API ポート `8080` 上で提供されます — 別ポートは開きません。web コンテナも `/mcp` をプロキシするので `https://<host>/mcp` でも到達できます。`YAGRA_MCP_ALLOWED_HOSTS` を設定している場合は web 側のホスト名を追加してください。追加しないとこの経路は拒否されます。

> ### TLS
>
> **WebUI は既定で HTTPS であり、平文リスナはありません**（ADR-044）。core は初回起動時に自己署名証明書を生成し、web コンテナが読む場所に書き出すので、まっさらなスタックはブラウザ警告つきで暗号化された状態で立ち上がります。**Settings ▸ TLS** で PEM のチェーンと鍵を貼るかアップロードすれば、何も再起動せずに数秒で切り替わります。
>
> 証明書の正本は PostgreSQL の 1 行です — 秘密鍵は KEK で封筒暗号化し、証明書チェーンは平文（証明書は構造上公開物のため）。ボリューム上のファイルはその行の材料化にすぎないので、削除しても安全です。
>
> - **平文からのリダイレクトは検討のうえ却下しました。** webhook 送信実装の多くはリダイレクトを追わず、追う実装は `POST` の `301` を `GET` に変えます — 受信イベントが何の痕跡も残さず届かなくなります。接続拒否のほうが正直な失敗です。
> - **`YAGRA_WEB_TLS=off`** は、外部のリバースプロキシやロードバランサが手前で既に HTTPS を終端している場合にのみ設定してください。
> - **暗号化された秘密鍵と PKCS#12（`.pfx`）は受け付けません。** 先に変換してください:
>   `openssl pkcs8 -topk8 -nocrypt -in key.pem -out key-plain.pem`、または
>   `openssl pkcs12 -in cert.pfx -nodes -out bundle.pem`。
> - **NATS バスの証明書は別物**で、Settings ▸ TLS は管理しません — NATS サーバが起動時に自分で読みます（下記 D）。

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

## A — 単一ノード, Docker（ビルド済みイメージ）<a id="a--単一ノード-docker-pull"></a>

**推奨するデプロイ構成であり、本ガイドの残りが前提とする構成です。** `docker-compose.deploy.yml` は GHCR から公開イメージを**取得**し（ローカルビルドなし）、`.env` で完全にパラメータ化され、保存済み監視資格情報が再デプロイを越えて維持されるよう永続 KEK を書き込む one-shot の `kek-init` を追加し、共有ログボリュームを core とポーラの**両方**が書ける状態にする one-shot の `log-init` を追加し（両者は別 uid で動き、イメージ側の所有者設定は Docker が空のボリュームを初めて用意するときしか効かない）、さらに **Settings ▸ Upgrade** を成立させる `yagra-updater` サイドカーを同梱します。2 つの `*-init` コンテナは起動後 `Exited (0)` で止まっているのが正常で、失敗ではありません。

リポジトリのチェックアウトは不要です — この compose ファイルは自己完結しており、参照する変数はすべて既定値を持ち、`/var/run/docker.sock` 以外のバインドマウントもありません:

```bash
mkdir yagra && cd yagra
curl -fsSL -o docker-compose.deploy.yml \
  https://github.com/horryworks/Yagra/releases/latest/download/docker-compose.deploy.yml
printf 'POSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 16)" > .env
docker compose -f docker-compose.deploy.yml up -d
```

**compose ファイルは `main` からではなくリリースから取ってください。** このファイルと、これが取得する
イメージは一体の成果物です。compose 側が、まだ公開されていないイメージにしか無いコマンド・環境変数・
初期化コンテナを要求することがあります。`releases/latest/download/` は最新の**安定リリース**に解決され、
これは `:latest` イメージタグの意味と同じなので、両者は必ず一致します。`main` にあるのは*次の*リリースの
compose であり、誰も取得できないイメージを指している場合があります。

`up -d` だけで取得も走ります — イメージに `pull_policy: always` が付いているため、別途 `pull` する必要はありません。起動したら **https://\<host\>/** を開きます（API は `:8080`）。証明書は Settings ▸ TLS で自分のものを取り込むまで自己署名です。

**compose ファイルは起動した場所に、この名前のまま置いておいてください。** 無停止アップグレードは自分自身のコンテナに付いた `com.docker.compose.project.working_dir` ラベルからディレクトリを読み戻し、そこに `docker-compose.deploy.yml` が無ければ実行を拒否します。compose の*プロジェクト名*は心配要りません（ファイル自身が `name: yagra` を固定しています）— 問題になるのはパスの方です。

**`POSTGRES_PASSWORD` は初回起動より前に設定してください**（上のスニペットはそうしています）。初期化時にデータベースのボリュームへ焼き込まれるため、後から変えるには `.env` の編集だけでなく `ALTER ROLE` が要ります。

`YAGRA_IMAGE_TAG` はイメージタグを選び、既定は `latest` です: `latest` は最新の**安定**リリース（プレリリースがこれを動かすことはありません）、`v<version>` タグは特定のリリースを固定、リリースの `<git-sha>` はそのビルドへの不変の参照（ロールバック = 古いタグで再実行）。公開されるのはリリースのみで、開発ビルドがレジストリに載ることはありません — つまり取得できるタグはすべてリリースです。

動いているコンテナが何から作られたかを知りたいときは、`docker exec yagra-core-1 cat /etc/yagra-source-ref` でコミットが、`/etc/yagra-build-profile` でコンパイルプロファイルが出ます。

**`.env` の設定** — 上のパスワード以外は任意です。全キーの説明は [`.env.example`](.env.example) にあります。注釈付きの版が欲しければ compose ファイルと並べて取得してください（`curl -fsSL .../.env.example -o .env`）。要点:

```ini
POSTGRES_PASSWORD=change-me            # 使い捨てでないマシンでは必ず変更
YAGRA_API_PORT=8080                    # API のホストポート（平文）
YAGRA_WEB_PORT=443                     # WebUI のホストポート（HTTPS）
# YAGRA_ADMIN_PASSWORD=choose-a-strong-password   # 未設定なら一度限りのランダム値をログ出力
# YAGRA_PUBLIC_DASHBOARD=false         # true = ログイン不要の読み取り専用ダッシュボード
# YAGRA_WEB_TLS=off                    # 手前のプロキシが既に HTTPS を終端している場合のみ
# YAGRA_API_BIND=127.0.0.1             # core の平文ポートを LAN から閉じる — 下記参照
```

**v0.1.22 より前からのアップグレード。** `YAGRA_WEB_PORT` の意味は変わっていませんが、そのポート上のスキームが変わりました。`.env` に `3000` が残っていればポートは 3000 のままで `https://<host>:3000` になります — そこで `http://` はもう応答しません。この行を削除すると `443` に乗ります。

**core の API ポートを閉じるのは 2 番目であって 1 番目ではありません。** `YAGRA_API_BIND=127.0.0.1` は平文 API を LAN から外し、TLS エッジだけを入口にします。ブラウザはどちらでも影響を受けません（web コンテナが `/api/` と `/mcp` を内部でプロキシするため）が、Prometheus の scrape・webhook 送信元・API スクリプトはこのポートを直接使います。**先に**それらを、信頼できる証明書のある `https://<host>/api/v1` へ移してください。同時にやると、全機械クライアントが原因 2 つ重なった状態で一斉に落ちます。

**資格情報の永続化（重要）。** `kek-init` サービスは 32 バイトの KEK を `kekdata` ボリュームへ一度だけ書き込み、以後は上書きしません。core はそれを `YAGRA_KEK_FILE=/kek/key` に読み取り専用でマウントします。永続 KEK が無いと core は再起動のたびに再生成される**一時**鍵にフォールバックし、保存済み資格情報（SNMP コミュニティ、API トークン）が再デプロイ後に復号できなくなります。compose がこれを配線済みなので、`kekdata` ボリュームを削除しないでください。

**アップグレード。** v0.2.2 以降、通常の手段は **WebUI の Settings ▸ Upgrade** です — この構成には全工程（バックアップ → 取得 → 対象イメージに同梱された compose 構成の適用 → 再作成 → 検証）を実行する `yagra-updater` サイドカーが同梱されており、シェルは不要です。この構成を **B** より優先する理由がこれです: 単一ノード構成のなかで、自分自身をアップグレードできるのはこれだけです。下のコマンドライン手順も引き続き有効で、サイドカーを止めている場合や動かせない場合の退路になります:

```bash
YAGRA_IMAGE_TAG=v0.2.5 docker compose -f docker-compose.deploy.yml pull
YAGRA_IMAGE_TAG=v0.2.5 docker compose -f docker-compose.deploy.yml up -d
```

マイグレーションは expand-contract 方式で自動実行され、`pgdata`/`vmdata`/`kekdata` は保持されます。[アップグレードとバックアップ](#アップグレードとバックアップ)を参照。

---

## B — 単一ノード, Docker（ソースからビルド）<a id="b--単一ノード-docker-build"></a>

開発・オールインワン用の構成で、**Yagra に手を入れる・監査する・独自ビルドを作る**ためのものです。`docker-compose.yml` はイメージをローカルで**ビルド**し（タグ `:dev`）、core・poller・web と 5 ストアすべてを 1 ホストで動かします。

```bash
git clone https://github.com/horryworks/Yagra.git
cd Yagra
docker compose up --build          # 単一ノードのフルスタックをビルドして起動
```

WebUI は **https://localhost:8443**（API は http://localhost:8080）。

ブラウザは警告を出します — 証明書は core が初回起動時に生成した自己署名のものです。いったん受け入れて、Settings ▸ TLS で正式な証明書を取り込んでください。（この開発用スタックが `443` ではなく `8443` を公開しているのは、ノート PC では `443` が埋まっていることが多く、rootless Docker は 1024 未満をそもそも公開できないためです。上記 **A** は `443` を使います。）

**初回ログイン。** `YAGRA_ADMIN_PASSWORD` は既定で未設定のため、core は一度限りのランダムな `admin` パスワードを生成し、ログに**一度だけ**出力します:

```bash
docker compose logs core | grep -i password
```

`admin` でログインして変更してください。自分で指定したい場合は `docker-compose.yml` の `core` サービスの `YAGRA_ADMIN_PASSWORD` をコメント解除します。

**稼働内容。** web はホスト `:8443`（HTTPS）、API は `:8080`（平文）。poller は syslog を `:514/udp`、SNMP トラップを `:162/udp` で受信。PostgreSQL/Redis/NATS/VictoriaMetrics は Docker 内部ネットワークに留まります。マイグレーションは core 起動時に自動実行され、手動手順はありません。名前付きボリューム `pgdata` / `vmdata` が `docker compose down`/`up` をまたいでデータを保持します。

⚠️ **業務で依存するシステムにこれを選ぶべきでない理由が 2 つあります。** どちらも見落としではなく意図的なものです:

- **自分自身をアップグレードできません。** ここには `yagra-updater` サイドカーが無く、この構成がビルドする `:dev` タグは公開もされないため、アップデータが移行できるリリースがそもそも存在しません。Settings ▸ Upgrade は、失敗すると分かっている操作ボタンを並べるのではなく、その旨を表示します。
- **KEK が揮発します。** 保存済みの秘密を暗号化する鍵が再起動のたびに再生成されるため、自己署名証明書は一緒に作り直されるだけですが、**取り込んだ**証明書は再起動後に復号できません。その場合 core はそれを明示したうえで、最後に材料化した証明書を配り続けます（勝手に自己署名へ差し替えることはしません）。正式な証明書の取り込みは、永続 KEK を持つスタック＝ **A** で行ってください。

> 開発・評価には十分な構成です。大切なデータを扱うなら **A**（公開イメージ、保存済み資格情報が再起動を越えて維持される永続 KEK、そして WebUI からの無停止アップグレード）を使ってください。

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
- **VictoriaMetrics** — `victoria-metrics-prod --retentionPeriod=12`（12 か月、単一ティア。[データ保持期間](#データ保持期間)を参照）
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

フルスタックを中央で（**A** のように）動かし、リモート拠点にポーラを追加します。各ポーラは拠点のデバイスをローカルにポーリングし、結果をバス経由で返します。ノードは `pool` 属性を持ち、core のコーディネータが各プールのノードをコンシステントハッシュで生存ポーラへ割り当て、障害時は自動フェイルオーバーします。

> **バスはデバイス資格情報を平文で運びます。** 単一ホストなら問題ありません（内部 Docker ネットワーク、何も公開しない）。バスがリモート拠点へ信頼境界を越える瞬間、**まず** TLS 暗号化と認証が必須になります。`:4222` を平文で公開しては**いけません**。

### ステップ 1 — リモートポーラーを受け入れる（WebUI）

**Settings ▸ Pollers ▸ リモートポーラー** で「リモートポーラーを受け入れる」を押します。拠点が接続する
ホスト名または IP アドレスを入力してください。この宛先がバス証明書に入ります。**証明書に入っていない
宛先へ接続する拠点は接続できません。**

中央でやることはこれだけです。Yagra は次を行います。

- その宛先を含むバス証明書を再発行する（証明書は初回起動時に自動生成済み。秘密鍵は他の秘密と同じく
  PostgreSQL に封筒暗号で保存され、バスが読むボリュームへ書き出されるだけ）
- バスの TLS とパスワード認証を有効にしてポートを公開し、同じ変更で同居する core とポーラーも
  `tls://` に移す
- 設定を `.env` に書く。**`.env` はアップグレードで保護されます** — 手で書き換えた compose ファイルは
  保護されません（下記）

> **1 分ほど監視が止まります。** NATS は TLS と平文を同時に提供できないため、バス・core・同居ポーラーの
> 3 つが作り直されます。先にデプロイ全体のメンテナンス期間が開くので通知は出ません。この画面はその間
> 切断されますが、これは異常ではありません。期間中のアラートは後追い記録されません（メトリクスは
> 記録されます）。

<details>
<summary>WebUI が使えない場合（シェルから設定する）</summary>

`docker-compose.deploy.yml` と同じ場所の `.env` に次を書き、
`docker compose -p yagra -f docker-compose.deploy.yml up -d` を実行します。WebUI が書くのと同じキーで、
**compose の編集は不要**です。

```ini
YAGRA_NATS_ARGS=-js -c /etc/nats/nats-server.conf
YAGRA_NATS_BIND=0.0.0.0
YAGRA_NATS_CORE_PASSWORD=強いコア用バスパスワード
YAGRA_NATS_POLLER_PASSWORD=強いポーラー用バスパスワード
YAGRA_CORE_BUS_URL=tls://core:強いコア用バスパスワード@nats:4222
YAGRA_POLLER_BUS_URL=tls://poller:強いポーラー用バスパスワード@nats:4222
YAGRA_BUS_CA_FILE=/etc/nats/certs/server-cert.pem
# バス証明書に追加する宛先（内部の既定名に追加されます）:
YAGRA_BUS_TLS_SANS=core.example.com,192.168.1.2
```

戻すときはこれらの行を消して、もう一度 up し直します。
</details>

> **`openssl` の手順が無くなった理由と、それが重要な理由。** 以前の手順は証明書を手で作り、
> `docker-compose.deploy.yml` を 2 か所書き換えるものでした。どちらも見た目より悪い状態でした。
> Settings ▸ Upgrade は**その compose ファイルを target イメージの中身で置き換える**ため、編集は
> 次のアップグレードで消えます。そして消えた後、**中央のスタックは正常に動き続け、リモートポーラーだけが
> 黙って接続できなくなります**。さらに、手順がマウントしろと言っていた `docker/nats/nats-server.conf`
> は**公開イメージに入っていません**でした。構成 [A](#a--単一ノード-docker-pull) で立てたデプロイには
> そのファイルが無く、バスが起動に失敗します。どちらも解決済みです — 設定ファイルは core イメージに
> 同梱されて自動でボリュームへ置かれ、切り替えは `.env` の変数で表現されます（アップグレードが保護します）。

### ステップ 2 — 拠点にトークンを発行して一式をダウンロードする

**Settings ▸ Pollers** の **トークン** 列に、そのポーラーが専用トークンを持つのか、デプロイ全体で共通の
ブートストラップシークレットを使っているのかが出ます。クリックして「トークンを発行してダウンロード」を
押してください。

拠点に必要なものが `.tar.gz` で 1 つ落ちます。`.env`（id・pool・バストークン・
`COMPOSE_PROFILES`）、`certs/server-cert.pem`（拠点が固定する証明書）、この core のイメージから
取り出した `docker-compose.poller.yml`、そして手順書です。**ポーラーがまだ存在しなくても構いません**
— トークンの発行が登録を兼ねるので、拠点でまだ何も動いていない段階で準備できます。

> **発行ダイアログの「この拠点にリリースを導入させる」チェック欄は、既定でオンです（v0.3.3 以降）。**
> この欄は `.env` に `COMPOSE_PROFILES=self-upgrade` を書きます。すると拠点でポーラーの隣に
> `yagra-poller-updater` が起動します。これは **root** で動き、その拠点ホストの Docker ソケットを
> 持つコンテナで、Settings ▸ Upgrade から拠点のポーラーを入れ替えられるようになります。発行前に
> チェックを外すか、あとから拠点の `.env` で `COMPOSE_PROFILES` を空にすれば、その拠点で
> ソケットを持つコンテナは 1 つも動きません。切り替えは compose ではなく `.env` で行ってください。
> アップグレードは compose を導入するリリースのものに入れ替えますが、`.env` には触れません。
> v0.3.3 より前にキットを渡した拠点は、再発行して渡すまで影響を受けません。

> **トークンはそのファイルの中だけにあります。** Yagra は SHA-256 のダイジェストしか保存しません。
> アーカイブを失くしたら新しいトークンを発行してください。発行した時点で古いものは無効になります。

利便性以外に、これで得られるものが 2 つあります。

- **登録されていないポーラー id は、どんなシークレットを示しても拒否されます。** 以前は id が自己申告で
  何とも照合されていなかったため、1 拠点の `.env` が漏れると**任意の id を名乗れました** — そして core が
  その id に送る working set には、その id に割り当てられたノードの SNMP コミュニティや API トークンが
  平文で入っています。
- **専用トークンを持つポーラーは、共通シークレットでは開けなくなります。** つまりトークンの発行は
  拠点ごとに爆風半径を狭める作業です。トークンを持たないポーラーは共通シークレットのままなので、
  アップグレードしても既存のフリートは動き続けます。トークン列はその状態を見るためのものです。

「トークンを失効」は、拠点を共通シークレットに戻します（漏洩後に新しいものを発行する前など）。
ポーラーの削除とは別物です — 削除はアンカーや履歴も一緒に消します。

### ステップ 3 — リモートポーラーを起動する

拠点のマシンで:

```bash
tar xzf yagra-poller-edge-tokyo-1.tar.gz
cd yagra-poller-edge-tokyo-1        # 展開した場所
docker compose -f docker-compose.poller.yml up -d
```

10 秒ほどで Pollers ページに現れ、core がそのプールのノードを割り当て始めます。

`docker-compose.poller.yml` は `network_mode: host` を使い（受動イベントの相関がデータグラムの
実際の送信元 IP を見られるように、また raw ICMP がホストのインタフェースに届くように）、`NET_RAW` を
付与します。

> **特権ポートの注意。** リモートポーラーは**非 root** で動く（ファイル capability の `NET_RAW` のみ）ため、`:514`/`:162`（1024 未満）を bind できません。既定の高いポート（`1514`/`1162`）を使ってホストのファイアウォールで転送する（`iptables … REDIRECT 514→1514`）か、機器から直接高いポートへ送ってください。

プールをスケールするには、同じ `YAGRA_POLLER_POOL`（と異なる `YAGRA_POLLER_ID`）でポーラーを増やします。core がプール内で再配分し、喪失時にはフェイルオーバーします。稼働ポーラーが 0 のプールはレガシーな都度 publish にフォールバックするので、ローリング更新中もノードが暗くなりません。

### ステップ 1 が同時に有効にするもの — ポーラーごとのバス資格情報（Auth Callout）

NATS の静的アカウントは `core` に全権を、`poller` に最小権限を与えます（publish は結果・イベント・
ハートビートのみ、subscribe は自分のジョブとワーキングセットのみ）。ただし **`poller` アカウントは
1 つの共有**なので、認証済みのポーラーはどのプールの割り当ても読めてしまいます。テナント境界では
ありません。

**NATS Auth Callout** がそれを塞ぎます。ステップ 1 がこれを有効にします。core がバスの認可
サービスになり、接続してくるポーラーごとに **その id 専用のスコープを持つ資格情報**を発行します。
ステップ 2 のポーラー別トークンを検査するのもこの経路です。署名鍵は初回起動時に生成されて
データベースに封じられ、公開鍵のほうは**バスの設定を書くのと同じ 1 回の実行**で書き込まれます。
**生成する物・マウントする物・写す物は 1 つもありません。**

知っておく価値のある結果が 2 つあります。

- **core はハートビートからポーラーの行を作り直さなくなります。** つまり **WebUI での削除が効く
  ようになります。** 裏返すと、ポーラーが台帳に載るのは**トークンを発行したとき**（ステップ 2）で
  あって、接続したときではありません。このコンポーズの中のポーラーは、ステップ 1 を ON にした
  ときに自動で登録されます。
- **この環境自身の core と poller は対象外**で、上の静的アカウントを使い続けます。この 2 つは
  固定の名前（`core` / `poller`）と、このホストの `.env` の中にしか存在しないパスワードを名乗り
  ます。**外から来る接続はすべてポーラー id を名乗るので、core が認可するか、拒否されるかの
  どちらかです。**

> **v0.3.2 より前に手で設定していても壊れません。** `YAGRA_NATS_CALLOUT_SEED_FILE` は生成された
> 鍵より優先されます。ただしどこかで外すことをおすすめします —— その手順は最後に
> `nats-server.conf` を編集する形でしたが、あのファイルは起動のたびにイメージから入れ直される
> ので、編集は次の起動までしか残りませんでした。

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
| `YAGRA_VM_WRITERS` | コア数（最大 4） | VictoriaMetrics へメトリクスを書くタスクの本数。ノード id でシャードするので 1 系列の順序は保たれる。キューと spill の上限は本数で割られる（増えない）。`1` で従来の 1 本構成に戻る |
| `YAGRA_RESULT_QUEUE_CAP` | `8192` | VictoriaMetrics が遅くなっている間、メトリクス層が抱えられるポール結果の件数。系列数が多いと VictoriaMetrics は周期的に失速し、この上限を超えたぶんは捨てられる（メトリクスは best-effort 層なので設計どおりだが、グラフの穴にはなる）。**変える前にこのデプロイの `yagra_vm_backlog_needed_high_water` を読むこと** —— 上限が無ければキューがどこまで伸びたかを表す値なので、そのままこの設定を決める数字になる。24 ポートで 1 件約 21 KB なのでメモリは線形に増える。`YAGRA_VM_WRITERS` の本数で割られる。131072 を超える値は丸められる（ログに出る） |
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
| `YAGRA_SMTP_HOST` / `_FROM` / `_TO` | 未設定 ⇒ メール無効 | 環境変数による SMTP アラートチャネル。3 つとも必須で、いずれかが欠けるか `_FROM`/`_TO` がメールアドレスとして解釈できない場合はチャネルを作りません |
| `YAGRA_SMTP_PORT` | `465`（暗黙 TLS） | SMTP ポート |
| `YAGRA_SMTP_USER` / `_PASS` | 未設定 ⇒ 認証なし | SMTP 認証情報。**両方**設定されたときのみ適用 |
| `YAGRA_POOL_COVERAGE_ALERT_AFTER_SECS` | `300` | ノードが残っているのに**生存ポーラーが 0 台**の状態がこの秒数続いたら critical アラートを上げる。ポーラーは終了時に自ら離脱を通知するため、ローリング再起動でも条件は即座に成立する — このデバウンスが「誰も呼び出さない」ためのもの。`0` でアラート無効（ゲージはどちらでも出力される） |
| **トラフィックフローと IP→ASN 補完** | | |
| `YAGRA_FLOW_RETENTION_DAYS` | `30`（1–3650 にクランプ） | フローの保持期間（日数）。**新規デプロイの初回起動時のみシード** — 以後は 設定 ▸ システム設定 ▸ データ保持期間 が正 |
| `YAGRA_CLICKHOUSE_SYSTEM_LOG_RETENTION_DAYS` | `7`（0–3650 にクランプ） | ClickHouse **自身**の `system.*_log` テーブルの保持期間。素の ClickHouse はこれらに TTL を一切与えないため、無制限に増え続け、自分をマージするために CPU を消費します。設定テーブルにシードされるのではなく、起動のたびに読まれます。**`0` にすると `system.*` に手を触れません** — `YAGRA_CLICKHOUSE_URL` がこのデプロイの持ち物でない ClickHouse を指している場合に使ってください |
| `YAGRA_IPASN_DB` | 未設定 ⇒ 補完無効 | フローの IP→ASN 補完に使うオフライン iptoasn.com TSV へのパス |
| `YAGRA_IPASN_RELOAD_SECS` | `0` ⇒ 起動時に一度だけ読み込み | IP→ASN ファイルのホットリロード周期（秒）。`>0` で再起動なしに再読み込み |
| **高可用性（HA）** | | |
| `YAGRA_ENABLE_HA` | `false` | PostgreSQL advisory lock によるオプトインのアクティブ/パッシブ リーダー選出 |
| `YAGRA_CORE_ID` | 未設定 | HA ログに出すこの core インスタンスの人間可読な識別子 |
| `YAGRA_SESSION_KEY_FILE` | 未設定 ⇒ プロセス内トークン | マウントした HMAC セッション署名鍵へのパス（セッションがどの core でも・再起動をまたいでも有効になる）。設定済みで読めない/不正なら起動失敗 |
| `YAGRA_PAT_OIDC_IDLE_DAYS` | `30` | **外部認証**アカウント（SSO **または** LDAP ディレクトリ）が所有する API トークンが、所有者がサインインしないまま有効な日数。IdP やドメインコントローラ側でのアカウント無効化は Yagra に通知されないため、所有者の沈黙が唯一の手がかり。ローカル/サービスアカウント所有のトークンは対象外。既存デプロイを壊さないため変数名は `OIDC` のまま（規則は外部種別すべてに適用）。1〜365 にクランプ |
| **MCP（AI クライアント）** | | |
| `YAGRA_ENABLE_MCP` | `false` | API ポート上の `/mcp` に MCP ツールサーフェスをマウント（認証は常に必須） |
| `YAGRA_MCP_ALLOWED_HOSTS` | 未設定 ⇒ 任意の `Host` を受理 | `/mcp` の `Host` ヘッダ許可リスト（カンマ区切り。DNS リバインディング対策） |
| **分析と RCA のレート上限** | | |
| `YAGRA_ANALYSIS_MAX_CONCURRENT` | `4` | 同時実行できるトラブルシュート分析の上限 |
| `YAGRA_ANALYSIS_RATE_PER_MIN` | `30` | 毎分受け付ける新規分析の上限 |
| `YAGRA_RCA_MAX_CONCURRENT` | `2` | 同時実行できる LLM 根本原因分析の上限（課金される外部呼び出し） |
| `YAGRA_RCA_RATE_PER_MIN` | `10` | 毎分受け付ける新規根本原因分析の上限 |
| `YAGRA_RCA_CACHE_SECS` | `900` | RCA レポートのキャッシュ寿命（秒）。`force` はキャッシュを迂回するが上限は迂回しない |
| `YAGRA_RCA_MAX_TURNS` | `6` | LLM 根本原因分析が回答に至るまでに使えるツール呼び出しのターン数上限。**`1` にすると v0.1.23 以前の単発動作に完全に戻る** — ツールは一切提示されず、プロバイダへのリクエストは以前とバイト単位で同一 |
| `YAGRA_RCA_TASK_BUDGET_SECS` | `240` | 1 回の根本原因分析（ツール呼び出し込み）の実時間上限。到達した場合はリクエストを失敗させず、モデルの最後の回答を返す |
| **WebUI からのアップグレード**（構成 **A**。下 4 つは core ではなく `yagra-updater` サイドカーが読みます） | | |
| `YAGRA_UPGRADE_DIR` | 未設定 ⇒ 実行側は無効 | core とサイドカーが要求を受け渡すディレクトリ（`docker-compose.deploy.yml` では共有ボリューム上の `/data/upgrade`）。この環境にアップグレード機構が**あるかどうか**を決めるのがこの設定です。未設定の場合、Settings ▸ Upgrade は「今動いているもの」とスキーマの状態は答えますが、「この環境は WebUI からアップグレードできません」と表示し、リリース一覧も適用ボタンもスイッチも出しません（リリース一覧の出所はサイドカーだけなので、移行先を答える手段がありません）。設定済みなのにサイドカーが応答しない状態とは別物で、そちらは環境の性質ではなく**異常**として表示されます。**このディレクトリの中身が実行されることは一切ありません** — 要求ファイル・ハートビート・アップロードされたアーカイブだけを置きます |
| `YAGRA_UPGRADE_BUNDLE_MAX_BYTES` | `4294967296`（4 GiB） | アップロードされるイメージアーカイブの上限。到着したバイト単位で判定します。リリース 3 イメージをまとめて save してもおよそ 1 GB なので、これは運用上の制限ではなく、**別のファイルをブラウザにドラッグしてしまったとき**に PostgreSQL と同じファイルシステムを埋める前に止めるための値です |
| `YAGRA_UPGRADE_REPO` | `ghcr.io/horryworks` | **リリース**を探しに行く先。`YAGRA_IMAGE_REPO` とは意図的に別の変数です — リリースの所在は、この環境が現在のイメージを取得した場所とは限らず、private ミラーから SHA タグのビルドを取得している環境では、リリースを 1 つも持たないレジストリを選択画面が見に行ってしまうためです。いずれにせよホスト側で固定されるので、API 要求がレジストリを指定することはできません |
| `YAGRA_UPGRADE_CHECK_SECS` | `86400`（1 日 1 回） | サイドカーが利用可能なリリースを一覧する間隔。一覧は選択画面を埋めるだけなので、頻繁に見に行っても得るものはありません。WebUI で機構を停止すると、この通信自体が止まります |
| `YAGRA_UPGRADE_ALLOW_BUNDLE` | `0` | アップロードされた `docker save` アーカイブからの導入を許可します。**Admin からホスト root への経路が「我々が公開した 3 イメージのいずれかのタグ」から「アーカイブに入っている任意のイメージ」へ広がります。** だからこそホスト側の設定で、WebUI からは有効にできません。到達できるレジストリが 1 つも無い環境でのみ設定してください。load 以降は通常経路と同一です — 同じバックアップ、同じ構成の差し替え、同じ provenance 検証を行い、アーカイブが操作者の名乗ったタグを含むことも照合します |
| `YAGRA_DOCKER_GID` | `0` | サイドカーの実行グループ。uid を `0` 以外にする場合にのみ意味を持ちます（root は gid に関わらずソケットに届きます） |
| **NATS Auth Callout（ポーラごとのバス資格情報）** | | |
| `YAGRA_NATS_POLLER_PASSWORD` | 未設定 ⇒ callout 無効 | **この 3 つのうち設定しうるのはこれだけで、しかもリモートポーラーのスイッチが自動で設定します。** この値が在ることが、core が callout 要求に応答する条件そのものです（自分のトークンを持たないポーラーが名乗る共有のブートストラップシークレットだからです）。NATS サーバ設定も静的 `poller` アカウント用に同じ値を消費します |
| `YAGRA_NATS_CALLOUT_SEED_FILE` | 未設定 ⇒ 保存された鍵を使う | **旧経路の上書き。** マウントした NATS アカウント nkey シードへのパス。v0.3.2 以降、core は初回起動時に署名鍵を自分で生成してデータベースに封じるので、マウントする物はありません。ここにパスを書けば今も優先されます（それ以前に設定していた環境のため） |
| `YAGRA_NATS_CALLOUT_ACCOUNT` | `$G` | 発行したポーラユーザを配置する NATS アカウント。バスの `callout.conf` が名乗るアカウントと一致必須ですが、**そのファイルはこの同じ値から書かれます**。ブローカのアカウントを独自に変えていない限り、触らないでください |
| `YAGRA_BUS_AUTH_CALLOUT` | 未設定 ⇒ 無効 | ポーラーのみ。`1` または `true` にすると、ポーラーは バスのユーザー名として自分の `YAGRA_POLLER_ID` を名乗ります。Auth Callout はこの名前で権限を絞ります。無効のまま（既定であり、リモートポーラーのスイッチが設定する状態）なら、`YAGRA_BUS_URL` に書かれた ユーザー名 —— 共有の静的アカウント —— を名乗ります。callout を有効にした環境でのみ ON にしてください。callout が無効な状態で自分の id を名乗ると、どの静的アカウントにも一致せずバスに拒否されます。 |
| **可観測性** | | |
| `YAGRA_DISK_WATCH_PATHS` | `/=root` | ホスト自己メトリクスが容量を報告するファイルシステム（カンマ区切りの `path` または `path=alias`）。core と poller の**両方**が読む |
| `YAGRA_OTEL_ENDPOINT` | 未設定 ⇒ ログのみ | OpenTelemetry トレース送出先の OTLP/HTTP エンドポイント（`OTEL_EXPORTER_OTLP_ENDPOINT` にフォールバック） |
| `OTEL_TRACES_SAMPLER` / `_ARG` | `parentbased_always_on` | トレースサンプラ。大規模時は `parentbased_traceidratio` + 引数（例 `0.01`）を使用 |
| `YAGRA_LOG_DIR` | 未設定 ⇒ stdout のみ | 1 時間ごとにローテートする JSON Lines ログの出力先ディレクトリ。stdout の**代わりではなく追加**で書く。`docker logs` に手が届かない環境向けで、これが無いとパニックや OOM の痕跡が一切残らない。サポートバンドルはこのファイルを HTTP 経由で読み戻す。`docker-compose.yml` / `docker-compose.deploy.yml` で既定設定済み — `.env` で空にすれば無効化。書き込みはノンブロッキングでポーリングループを止めず落とす。ディレクトリが書けない場合は起動失敗ではなく警告のうえ stdout のみに縮退 |
| `YAGRA_LOG_RETAIN_HOURS` | `48` | `YAGRA_LOG_DIR` に保持する時間別ログファイル数。自動で刈られるので、無人環境が自分のログでボリュームを埋めることはない |
| `RUST_LOG` | `info` | ログレベル（例 `info,yagra_core=debug`） |

### Yagra-poller

| 変数 | 既定 | 用途 |
|---|---|---|
| **識別子とバス** | | |
| `YAGRA_BUS_URL` | 未設定 ⇒ アイドル | NATS バス URL（ポーラが張る唯一のバックエンド接続） |
| `YAGRA_POLLER_ID` | `docker-compose.deploy.yml` では `local`、単体ではホスト名、無ければ `poller-<hex>` | 安定・一意・subject 安全なポーラ識別子。core にも同じ値が渡るので、両者が同じポーラーを指す |
| `YAGRA_POLLER_POOL` | `default` | このポーラが**最初に所属する**プール。v0.3.4 以降、初回接続より後の所属は core が持つため、Settings ▸ Pollers での移動はコンテナを作り直しても戻らない |
| `YAGRA_BUS_CA_FILE` | 未設定 ⇒ 平文 | `tls://` バスに固定する CA/サーバ証明書 |
| `YAGRA_MAX_CONCURRENT_POLLS` | `256` | 同時実行プローブ数の上限。**速さの上限ではなく「同時に何本走らせるか」** — 得られる polls/s はこの数 ÷ 1 本あたりの所要時間。この既定値で、ポーラー 1 台が **ICMP ノード 5 万台を 30 秒間隔**（1,675 polls/s）で取りこぼし無く回すことを実測済み（CPU 11〜14%）。変える前に `yagra_poll_demand_per_second` を見ること —— `rate(yagra_poll_jobs_executed_total[5m])` をこれで割ると「設定した監視の何割が実際に行われているか」になる。`yagra_poll_cycles_missed_total` は 1 周期まるごと過ぎたときにしか動かず、要求の 2% しか回せていない状態でも 0 だった。併せて `yagra_poll_inflight` も —— この数を上げて効くのは、そのゲージが上限に張り付いているときだけ。v0.3.5 から permit は**実際に探っている間だけ**握るので、この数は「機器の順番待ちをしているジョブ」ではなく本当に探りの本数を縛る。同じ枠で SNMP のテーブル取得も走るので、小さな拠点では下げる **見積もり方:** 1 台のポーラーが出せるのは `permit ÷ 1 ポールが permit を握っている時間` polls/s で、これは近似ではなく恒等式（256 / 512 / 1024 の 3 点で実測、いずれも占有は上限の 99.5% 以上）。どちらの項も `/metrics` にある —— 必要な量は `yagra_poll_demand_per_second`、保持時間は `yagra_poll_phase_seconds_sum{phase="execute"}` と `{phase="publish"}` の和を `_count` で割った値。⇒ `permit ≥ 要求 polls/s × 保持時間`。⚠️ **保持時間は自分のデプロイの `kind` ごとの値を、実際に走らせる領域で測ること** —— これは Yagra ではなく機器の答え方で決まり、フリート全体の平均は「どのチェックがどれだけ配られたか」の構成比で動く。ラボの実測では、答える機器は ICMP 4.3 ms・SNMP スカラー 5.3 ms・インタフェーステーブル 596 ms。答えなくなった機器は担当する**全チェック**でタイムアウトを丸ごと払う（ICMP 1 秒、SNMP の walk はおおむね 4 秒、MAU walk は 10 秒）。フリート全体ではおよそ 20 倍の差になるので、**黙ってしまったフリートではこの値を上げるよりポーラーを増やすほうが正しい**。 |
| `YAGRA_ADOPT_RATE_PER_SEC` | `200` | 他ポーラーの作業を引き継ぐ際のジッタ窓を決めるレート（チェック数/秒）。`0` で間隔全体にジッタ（従来動作）|
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
| `YAGRA_LOG_DIR` | 未設定 ⇒ stdout のみ | ポーラ自身の 1 時間ローテートログの出力先であり、**このポーラがサポートバンドルに載るかどうかを決めるスイッチ**でもある。同梱の compose は意図的に 2 通りに設定している。**同居**ポーラには `/var/log/yagra/pollers`（core の共有ログボリュームのサブディレクトリ。core がディスクから読み戻す）、**リモート拠点**のポーラ（`docker-compose.poller.yml`）には自分専用ボリューム上の `/var/log/yagra`（要求に応じて一定期間分をバス経由で送る）。未設定なら stdout のみに出力し、`log-ship` 能力を宣言しないので、バンドルは待たずに「未収録」と記録する |
| `YAGRA_LOG_RETAIN_HOURS` | `48` | `YAGRA_LOG_DIR` に保持する時間別ログファイル数。自動で刈られる。core とは**別枠**なので、ポーラのログが core 自身のログを押し出すことはない |
| `RUST_LOG` | `info` | ログレベル |

> **compose 専用の変数**は Docker Compose / NATS 設定が消費するもので、Rust バイナリは読みません — バイナリが見るのは最終的に組み立てられた `YAGRA_BUS_URL` などだけです。`.env.example` を参照:
>
> - イメージとストア: `YAGRA_IMAGE_TAG`, `POSTGRES_PASSWORD`
> - ホストポートのマッピング: `YAGRA_API_PORT`, `YAGRA_API_BIND`, `YAGRA_WEB_PORT`, `YAGRA_SYSLOG_PORT`, `YAGRA_TRAP_PORT`, `YAGRA_FLOW_PORT`, `YAGRA_SFLOW_PORT`, `YAGRA_NATS_PORT`
> - WebUI の TLS: `YAGRA_WEB_TLS`（compose）、`YAGRA_TLS_DIR`（core — 証明書の材料化先）
> - バスの TLS + auth（D）: `YAGRA_CERT_DIR`, `YAGRA_NATS_CORE_PASSWORD`, `YAGRA_NATS_POLLER_PASSWORD`（core も Auth Callout のブートストラップシークレットとして読み、callout を動かすかどうかもこれで決まる）。v0.3.2 以降 `YAGRA_NATS_CALLOUT_ISSUER` は**意図的にありません** —— アカウント公開鍵は、バスの残りの設定を書くのと同じ 1 回の実行で `callout.conf` に直接書かれるので、アップグレードをまたいで食い違うことがありません
> - マウントする鍵ディレクトリ: `YAGRA_SESSION_KEY_DIR`（`YAGRA_SESSION_KEY_FILE` 用の `session.key` を置く）。`YAGRA_CALLOUT_SEED_DIR`（`YAGRA_NATS_CALLOUT_SEED_FILE` 用の `account.seed`）は旧経路で、今は不要です
> - ポーラのログ出力先: `YAGRA_POLLER_LOG_DIR`（既定 `/var/log/yagra/pollers`）— `docker-compose.deploy.yml` が同居ポーラの `YAGRA_LOG_DIR` として渡す値。空にすればそのポーラは stdout のみになる
> - IP→ASN 更新サイドカー: `YAGRA_IPASN_URL`（データセット URL）, `YAGRA_IPASN_REFRESH_SECS`（取得周期。既定 `604800` = 週次）
> - 拠点の自己アップグレード (D): `COMPOSE_PROFILES` — Docker 自身の変数で、監視拠点では「その拠点がリリースを導入できるか」を決めるスイッチです。`self-upgrade` が入っていると `yagra-poller-updater` サイドカーが起動し、ポーラーがその能力を名乗ります。値を空にすれば、その拠点で Docker ソケットを持つコンテナは 1 つも動きません。発行されたキットがこれを書きます

---

## 分散トレーシング（OpenTelemetry）<a id="分散トレーシング"></a>

各バイナリは構造化ログと Prometheus `/metrics` を標準で出力します。**分散トレーシングはオプトイン**です: `YAGRA_OTEL_ENDPOINT`（または標準の `OTEL_EXPORTER_OTLP_ENDPOINT`）に OTLP/HTTP コレクタを設定すると、core と poller が 1 回のポーリングをエンドツーエンドで繋ぐ span（core の dispatch → poller の poll → core の ingest）＋北向き API リクエストごとの span を送出します。未設定なら**トレーシングのオーバーヘッドはゼロ**（ログのみ）で、単一構成 MVP はコレクタ不要です。

- **ローカルで試す:** `docker compose --profile tracing up` で同梱の Jaeger（UI は http://localhost:16686）が起動します。次に `docker-compose.yml` の `core` と `poller` の**両方**で `YAGRA_OTEL_ENDPOINT: http://jaeger:4318` をコメント解除します。
- **大規模時はサンプリング。** 数万ノードが間隔ごとにポーリングすると 1 ポーリング＝1 トレースになります。`OTEL_TRACES_SAMPLER=parentbased_traceidratio` と `OTEL_TRACES_SAMPLER_ARG=0.01`（1%）を設定してください。`parentbased_*` は core⇄poller をまたいでトレース全体の判定を一貫させます。トレースコンテキストはバス上の `trace_context` フィールドで運ばれ、**トレーシング無効時は wire に出ず**、N-1 ピアは無視します（N/N-1 安全）。
- **本番:** エンドポイントは、バックエンド（Tempo, Jaeger, Honeycomb など）へ転送する OpenTelemetry Collector に向けます。リモート拠点のポーラは、NATS バスとは別に、到達可能な独自のコレクタエンドポイントが必要です。

---

## アップグレードとバックアップ<a id="アップグレードとバックアップ"></a>

アップグレードは低コストで、データを**決して**失わず・壊さないよう設計されています:

- **Settings ▸ Upgrade が代わりに実行します — これが通常のアップグレード手段です（v0.2.2 以降、構成 **A**）。** それ以外のインストール方法 — ソースからのビルド、ネイティブ実行、`yagra-updater` サイドカーを持たない構成 — にこの機構はありません。その場合、画面は失敗すると分かっている操作ボタンを並べるのではなく、その旨をはっきり表示します。移行できるリリースが画面に並び、バックアップ → pull → 対象イメージの中にある構成の導入 → 再作成 → 検証までを実行します。実行するのは `yagra-updater` サイドカーで、Docker ソケットを持つのはこのコンテナだけです（core は持ちません）。アップグレードの要求には **manage-the-deployment（デプロイの管理）** が必要で、これを持つのは Admin だけです。監査に記録され、MCP には出していません。同じ画面のスイッチで機構ごと停止できます。設定は PostgreSQL に保存されるため、それが管理するアップグレードをまたいで残ります。
  - **現在より古いリリースはダウングレードで、実際に起動できる場合にのみ選べます。** マイグレーションは**互換下限**（適用後にそれ以上でしか動かなくなる版）を宣言でき、現在の下限を下回るリリースは隠さず、理由を添えて選択不可の状態で表示します。戻しても 1 行も失われません — 新しい版が追加した列はそのまま残り、読まれないだけです。
  - **レジストリに到達できない場合。** イメージに到達できる場所でリリース 3 イメージを `docker save` し、同じ画面からアーカイブをアップロードしてください。ホスト側の `YAGRA_UPGRADE_ALLOW_BUNDLE=1` という 2 段目のオプトインが必要です — `docker load` はアーカイブに入っているものを何でも導入するためで、有効化の前に下記の変数リファレンスを読んでください。
- **DB マイグレーションは expand-contract で、core 起動時に自動実行**されます。N→N+1 は常にサポートされます。`yagra-core migrations` は、そのバイナリに埋め込まれたマイグレーション一覧を DB も設定も無しで JSON 出力するので、**対象イメージの中で先に実行して**何も触る前に計画を立てられます。
- **バスはバージョン耐性（N/N-1）があります。** 新しい core は古いポーラとロールアウト中も動作するため、core を先に、ポーラを後にアップグレードできます。
- **ローリングアップグレード。** ポーラはステートレスなので任意の順で入れ替え可能です。Docker なら新タグを取得して `up -d`（**A** 参照）。リモートポーラは拠点ごとに取得して `up -d`。一時的に落ちたプールはレガシー publish にフォールバックするため、ノードは止まりません。
- 🚨 **v0.3.4 より前に立てた遠隔拠点は、次に中央をアップグレードする前に、拠点で 1 度だけ作り直してください。** その拠点のデプロイディレクトリで実行します:

  ```bash
  docker compose -p yagra-poller -f docker-compose.poller.yml up -d
  ```

  拠点のアップデーターは**名前付き**サービスなので、apply が作り直すのは `poller` だけで、アップデーター
  自身は作り直されません —— コンテナは作られたときの定義を持ち続けます。v0.3.4 より前に作られた
  アップデーターは `docker compose` を自分のコンテナの中から実行するため、構成が相対パス `./certs` で
  マウントしている証明書ディレクトリが、**そのコンテナの中にしか存在しないパス**として解決されます。
  Docker は存在しないホストパスを失敗させず**空で作る**ので、入れ替わったポーラーはバスを信頼する材料を
  何も持たずに起動し、二度と接続できません —— それでいて拠点は `apply … succeeded`、Settings ▸ Upgrade は
  「揃っている」と表示します。他に生きたポーラーが居るプールはノードが移るので監視は続きますが、
  **1 台だけのプールは手で直すまで暗いまま**です。v0.3.4 以降のアップデーターは、このマウントを
  ホスト側のディレクトリに対して解決し、証明書ディレクトリが空のポーラーは作り直しを拒否するので、
  必要なのはこの 1 回だけです。
  - ⚠️ **先にその拠点の `.env` の `YAGRA_IMAGE_TAG` を確認してください。** アップデーターはタグを自分の
    コマンドラインで渡すため、`.env` がレジストリに無いもの（開発ビルドなど）を指したままでも誰も
    気づきません。一方この作り直しを**手で打つと `.env` が読まれます。** いま動いているリリースに
    合わせておかないと、pull の時点で失敗します。
- **大きなアップグレードの前にバックアップを取得** — 下記[バックアップと復元](#バックアップと復元)を参照。

---

## バックアップと復元<a id="バックアップと復元"></a>

Yagra はバックアップ製品を作りません。PostgreSQL・VictoriaMetrics・ClickHouse はいずれも成熟した
手段を持っており、その上に Yagra 独自のオーケストレーションを重ねると、**復元手順が「バックアップを
取った時の Yagra のバージョン」に依存する**ようになります。代わりに Yagra が提供するのは、スクリプト化
された手順と、**そのバックアップが本当に復元できることを検証するスクリプト**です。

### 何をバックアップするか

| ティア | データ | 必須？ |
|---|---|---|
| **1 — 失ってはいけない** | **KEK**（`kekdata` / `YAGRA_KEK_FILE`）、PostgreSQL（`pgdata`）、VictoriaMetrics（`vmdata`） | **はい** |
| 2 — TTL 付き・喪失許容 | VictoriaLogs（`vldata`、30 日）、ClickHouse フローストア（`chdata`、30 日） | いいえ — 自身の期限で消え、保全必須の状態を持たない |
| 3 — 再構築可能 | Redis | いいえ — PostgreSQL が持つ状態のミラー |

**PostgreSQL のダンプは設定の一部ではなく全体です**: ノード・フォルダ・プロファイル・閾値・分類ルール・
通知チャネル・ルーティングルール・転送先・URL/DNS チェック・ユーザー・アラート履歴・監査ログ。

> **KEK が第 1 項目であり、データベースのダンプとは別の場所に保管してください。**
> KEK を失うと、保存済みの監視資格情報はすべて永久に復号不能になります — 完璧に復元できるのに何も
> ポーリングできないデータベースです。両方を同じ場所に置けば、その場所を壊す 1 回の障害が両方を壊します。
> `YAGRA_SESSION_KEY_FILE` と `YAGRA_NATS_CALLOUT_SEED_FILE` も設定していれば一緒に取得してください。

### 取得

```bash
./scripts/yagra-backup.sh /srv/backups/yagra          # ティア1 一式 + manifest を書き出す
```

メトリクスのスナップショットだけは、取得できなくても実行そのものは失敗しません（メトリクス
ストアの無い環境でも設定のバックアップは必要なため）。v0.2.4 からは、そのスキップを**推測では
なく明示**します。manifest が `metrics_snapshot`（スナップショット名、または `null`）を持ち、
末尾のサマリーが「このバックアップにメトリクスは入っていない」旨を文章で報告します。バックアップ
を完全とみなす前に、この行を確認してください。

### 検証 — こちらが本体

```bash
./scripts/yagra-restore-verify.sh /srv/backups/yagra/yagra-backup-<stamp>
```

**使い捨ての** compose プロジェクト（`yagra-verify`。終了時に `down -v`、本番のプロジェクト名を指すと
実行を拒否）へ復元し、次の 4 点を assert します:

1. `/readyz` が 200 — 復元したデータで core が起動する
2. ノード数が manifest と一致 — 設定が戻っている
3. **すべての資格情報が実際に復号できる** — KEK が戻り、かつ暗号文と一致している
4. `audit_log` の行数が一致 — 「誰が何を変更したか」の証跡が生存している

3 は他から推測できない唯一の項目です。復元は完璧に見えていても鍵が別物ということがあり、次のポーリングが
失敗するまで誰も気づきません。`GET /api/v1/credentials/health` でいつでも確認できます。資格情報が 0 件の
バックアップは PASS ではなく **SKIPPED** を報告します。

**破壊的マイグレーションの前に検証を実行してください**（組込カタログの再シード、マイグレーション
`0020`–`0022`）。ADR-017 がロールバック手段を要求しているのはこの場面です。

### 復元は前方のみ

復元先は**バックアップ元と同じか、それ以降のバージョン**でなければなりません（マイグレーションは前進のみ）。
**ダウングレード復元は非サポート**で、検証スクリプトは黙ってデータを壊す代わりに実行を拒否します。

---

## 設定バンドル（デプロイ間で設定を移す）<a id="設定バンドル"></a>

バックアップが復旧するのは**このデプロイ**です。**設定バンドル**はもう一方の用途 —
あるデプロイで作った監視設定を別のデプロイに適用する（検証環境から本番へ、旧サーバから新サーバへ）
ためのものです。**設定 ▸ 設定バンドル**、または `GET`/`POST /api/v1/config/bundle`。
書き出し・取り込みとも管理者のみです。

```bash
# 移行元から書き出す。自己署名の初期証明書のままなら --cacert <file>（評価中なら -k）を付ける。
curl -sS -H "Authorization: Bearer $TOKEN" \
     https://source/api/v1/config/bundle > bundle.json

# 移行先で何が起きるか確認する（実際の取り込みを行ってロールバック）
curl -sS -X POST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     --data-binary @bundle.json \
     'https://target/api/v1/config/bundle?dry_run=true'

# 適用する
curl -sS -X POST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     --data-binary @bundle.json https://target/api/v1/config/bundle
```

**バンドルはバックアップではありません。** 秘密・メトリクス・イベント・履歴のいずれも含みません。
設定の複製にはこちらを、サーバ喪失からの復旧には上のスクリプトを使ってください。

### 含まれるもの

デバイスプロファイル、メトリクスセットとその紐付け、分類ルール、ノードグループ、ノード、閾値、
URL/DNS 監視、転送先、イベントソースとイベントルール、レポートテンプレートとスケジュール、
分析スケジュール、グローバルのポーリング既定値。

### 意図的に含まれないものと、その理由

| 含まれないもの | 理由 |
|---|---|
| 資格情報 | このデプロイの KEK で封印されているため。バンドルが運ぶのは **id だけ**で、取り込み側は同じ id が既に存在する場合にのみ参照を維持します。 |
| 通知チャネルとルーティングルール | チャネルの実体は封印された設定そのもので、既存チャネルの id に設定を後から付ける API がありません。＝取り込んだチャネルは決して動かせず、それを指すルールは**黙って誰にも通知しない**状態になります。移行先でチャネルを作り直してからルールを作ってください。 |
| ユーザ・API トークン・OIDC プロバイダ | アイデンティティ。取り込みは書き込み経路なので、アカウントを運べると「設定の復元」が権限昇格の最短経路になります。 |
| ダッシュボード | ウィジェットがノード/グループ参照を埋め込んでおり取り込み側で検証できないため、運ぶと無言で壊れた表示になります。 |
| 保持期間 | **移行先**の方針（ディスク・コンプライアンス要件）であり、短縮はデータ削除を意味します。取り込みが勝手に変えてよいものではありません。 |
| Meraki / LLM / ポーラ / MIB 設定 | プロバイダ資格情報、あるいは移す設定ではなく移行先デプロイ自身の性質だから。 |
| メトリクス・イベント・フロー・アラート履歴 | 時系列とイベントの階層。本機能のスコープ外で、各ストア自身の移行手段があり容量も GB 級です。 |

組込のプロファイル・メトリクスセット・分類ルール・トラップルールも含まれません（移行先が同じ予約 id
で自分で seed するため）。

### 取り込みの挙動

**upsert のみ。** 同じ id の行は更新、無い行は作成。**削除は一切行わず、置換モードもありません** —
フラグ 1 つの距離にあり、それこそが取り込みを回復不能にするフラグだからです。全体が 1 トランザクション
なので、失敗しても何も残りません。

レポートはスキップ・変更した行をすべて報告します。

- 必須の参照が移行先に無い行は**スキップ**され、決して広げられません（存在しないノードに紐付いた
  イベントルールは、参照を外すとフリート全体にマッチしてしまいます）
- 任意の参照が無い場合は**解除**し、件数を報告します
- 秘密を必要とする転送先や Webhook ソースは**無効状態**で作成されます。移行先で秘密を再入力
  （またはトークンをローテート）してから有効化してください。移行先に既に動く秘密がある場合はそれを
  維持し、有効/無効の状態も変更しません
- スケジュールの次回実行時刻は移行先の時計で**再計算**されます

`?dry_run=true` は取り込み全体を実行してロールバックするので、レポートは適用した場合とまったく同じ
内容です。**先に実行してください。**

### サイズ上限

バンドルは 1 つの JSON 文書なのでフリートと共に大きくはできません。いずれかのテーブルが 10,000 行を
超えると、書き出しは**切り詰めずに拒否**します（完全に見える不完全な設定を作らないため）。その規模の
デプロイは災害復旧の領域で、上の `pg_dump` の担当です。

---

## データ保持期間<a id="データ保持期間"></a>

各ストアがどのデータをどれだけ保持するか。大半は **設定 ▸ システム設定 ▸ データ保持期間** で変更でき、
再起動なしに次回の掃引から適用されます。

| データ | ストア | 既定 | 変更方法 |
|---|---|---|---|
| アラート履歴・ノード状態スナップショット・DNS チェーンの変化・マッチ済みイベント | PostgreSQL | 90 日 | 設定画面 |
| 未マッチの受動イベント | PostgreSQL | 24 時間 | 設定画面 |
| レポート実行 | PostgreSQL | 90 日 | 設定画面 |
| トラフィックフロー | ClickHouse | 30 日 | 設定画面 |
| イベントログ | VictoriaLogs | 30 日 | **コンテナのフラグ**（下記） |
| メトリクス | VictoriaMetrics | 12 ヶ月 | **コンテナのフラグ**（下記） |
| 監査ログ | PostgreSQL | **無期限に保持** | 設計上、prune しない |

**監査ログは prune されません。** 誰が何を変更したかを、ログ掃除のついでに消してはならないためです。

### Yagra が変更できない 2 行

VictoriaMetrics と VictoriaLogs は保持期間を**プロセスの起動フラグ**として受け取り、実行時に変更する API を
持ちません。そのため Yagra はこの 2 つを（各ストアの `/flags` から読み戻して＝**実際に効いている値**を）
表示しますが、設定はできません。変更はデプロイの編集になります:

```bash
# docker-compose.deploy.yml
#   victoriametrics: command: ["--retentionPeriod=24"]   # 月数
#   victorialogs:    command: ["-retentionPeriod=90d"]
docker compose -p yagra -f docker-compose.deploy.yml up -d victoriametrics
```

編集したコンテナだけが再作成され、スタックは動いたままです。なお**期間を短くしても即座に削除されるわけでは
ありません** — VictoriaMetrics / VictoriaLogs はストレージのマージが進む過程で期限切れデータを落とします。

行が「不明 — ストアが保持期間のフラグを報告しませんでした」と表示される場合、フラグが設定されておらず
製品の組込既定で動作しています。Yagra は数値を推測せず、そのことを表示します。

`YAGRA_FLOW_RETENTION_DAYS` はフローの保持期間を**新規デプロイの初回起動時にのみ**シードします。以後は
設定画面の値が正で、変更は ClickHouse へ即時反映されます（`ALTER TABLE … MODIFY TTL`）— 従来は効果が
無かった既存ボリュームに対しても適用されます。

---

## アンインストール<a id="アンインストール"></a>

Yagra はホストに何もインストールしません — パッケージも、システムサービスも、デプロイ用ディレクトリと
Docker 自身の保存領域の外にあるファイルも作りません。アンインストールは、インストールを逆にたどるだけです。

**以下はすべて `docker-compose.deploy.yml` を置いたディレクトリで実行し、毎回 `-p yagra` を付けて
ください。** 付け忘れると compose はプロジェクト名をディレクトリ名から作り、**別の空のプロジェクト**に
対して動きます。`down` は「成功」と表示しますが何も消えず、本物のスタックは動き続けます。

### 止めるだけ（データは残す）

```bash
docker compose -p yagra -f docker-compose.deploy.yml down
```

コンテナとネットワークは消え、名前付きボリュームはすべて残ります。`up -d` すれば、設定・アラート
履歴・メトリクスをそのまま持った同じデプロイが戻ります。

### 完全に消す

```bash
docker compose -p yagra -f docker-compose.deploy.yml down -v --remove-orphans
```

`-v` は名前付きボリューム 11 個を破棄します。これは元に戻せません。

| ボリューム | 失われるもの |
|---|---|
| `pgdata` | ノード・ユーザ・閾値・アラート履歴・確認応答・すべての設定 |
| `vmdata` | メトリクスの全履歴 |
| `vldata` | 受動イベント（syslog / トラップ / Webhook）の全件 |
| `chdata` | トラフィックフローの全レコード |
| `kekdata` | **鍵暗号化鍵（KEK）** — 下記参照 |
| `tlsdata` / `buscerts` | 書き出された証明書（どちらも PostgreSQL の行の写し） |
| `logdata` / `pollerbuf` / `upgradedata` / `ipasndata` | ローテートしたログ・store-and-forward バッファ・アップグレードの受け渡し・IP→ASN データセット |

**バックアップから戻せないのは `kekdata` だけです。** 保存済みの監視資格情報 — SNMP コミュニティ、
SNMPv3 認証情報、API トークン、バスの秘密鍵 — はすべてこの鍵で封筒暗号化されており、鍵は再生成
できません。KEK なしで取ったデータベースのダンプは、**完璧に復元できるのに何もポーリングできません。**

戻ってくる可能性が少しでもあるなら、`-v` で消える前に KEK を取り出してください。

```bash
docker run --rm -v yagra_kekdata:/kek busybox cat /kek/key > yagra-kek.bin   # 32 バイト
```

⚠️ **[設定バンドル](#設定バンドル)は代わりになりません。** バンドルは資格情報を一切運ばず、id だけを
運びます。しかも移行先が既にその id を持っている場合にしか参照を保ちません。新しい KEK で作り直した
デプロイにその id は存在しないので、参照は落ちます。階層ごとの一覧は[バックアップと復元](#バックアップと復元)を参照。

### イメージとディレクトリも消す

```bash
docker compose -p yagra -f docker-compose.deploy.yml down -v --rmi all --remove-orphans
cd .. && rm -rf yagra          # compose ファイルと、POSTGRES_PASSWORD が入った .env
```

`--rmi all` は全サービスのイメージを消します。バックエンドのストア（`postgres` / `redis` / `nats` /
`victoria-metrics` / `victoria-logs` / `clickhouse` / `busybox` / `alpine` / `docker:28-cli`）も
対象です。他のプロジェクトがまだ使っているものは Docker が自動的に飛ばします。

### リモートポーラは別のスタックです

リモート拠点に置いたポーラ（構成 **D**）は、そのホスト上の独立した Compose プロジェクトです。上の
操作では一切消えず、中央スタックが無くなったあともバスへの再接続を延々と試み続けます。**各ホストで
個別に**消してください。

```bash
docker compose -f docker-compose.poller.yml down -v
```

### 消し残りの確認

compose ファイルを失くした場合や、何も残っていないことを確認したい場合は、Compose が付けたラベルで
引けます。

```bash
docker ps -a     --filter label=com.docker.compose.project=yagra
docker volume ls --filter label=com.docker.compose.project=yagra
docker network ls --filter label=com.docker.compose.project=yagra
```

出てきたものは `docker rm -f` / `docker volume rm` / `docker network rm` で消せます。

WebUI にアンインストール操作はありません。**設定 ▸ アップグレード**はリリース間の移動だけを行います。
アンインストールは意図的にホスト側の作業にしてあります。

---

## ディレクトリでのサインイン（LDAP / Active Directory）

**設定 ▸ 認証 ▸ ディレクトリ (LDAP/AD)** で設定します（ADR-041）。有効化する環境変数はありません —
設定が空である状態がオフであり、保存されるまで一切接続しません。

**サインインの流れ。** UI は通常のログインフォームだけです。Yagra はまず自分の `users` テーブルを
引き、ローカルアカウントならローカルで完結してディレクトリには一切触れません。そうでなければ
サービスアカウントで対象者を検索し、**2 本目の独立した接続**で、その検索が返した DN でバインドします。
入力名から DN を組み立てることはしません — 想定外の OU 構成で必ず壊れるからです。

**ローカル管理者を必ず 1 つ残してください。** この節で最も重要な点です。ローカルアカウントが先に
試されるのでディレクトリが落ちても締め出されませんが、その保護が働くのは**ディレクトリを知っている
バイナリが動いている間だけ**です。管理者が全員ディレクトリアカウントの状態でリリースをロールバック
すると、誰もサインインできなくなります。

| 設定項目 | 内容 |
|---|---|
| 通信方式 | **LDAPS**（接続時から TLS、通常 636）か **StartTLS**（通常 389）。平文の選択肢は意図的にありません — bind パスワードが警告も無く平文で流れるためです。 |
| CA 証明書 | 社内 CA の PEM。ほぼ必須です（社内 AD はコンテナのバンドルが信頼しない証明書を提示します）。**証明書検証を無効化する手段は無く、今後も作りません** — CA を設定するのが正規の解決策です。 |
| サービスアカウントの DN / パスワード | 検索段でのみ使用。保存時は封筒暗号（ADR-018）で、API から返ることはありません。空にはできません: DN 付き・パスワード空の bind は*匿名 bind* であり、多くのディレクトリが成功を返します。 |
| ユーザー検索フィルタ | `{username}` を必ず含めます。無いとベース DN 配下の全エントリに一致します。AD 既定: `(&(objectClass=user)(sAMAccountName={username}))` |
| ユーザー名の属性 | Yagra のユーザー名として保存される正規の名前。AD なら `sAMAccountName`。入力値ではなくディレクトリ側から取ります — AD は大文字小文字を無視して照合しますが、Yagra の username 列は区別するためです。 |
| 識別子の属性 | エントリ固有の不変 ID。AD は `objectGUID`、OpenLDAP は `entryUUID`。改名しても別アカウントにならないのはこれのおかげです。返さないエントリは、空の ID で保存せず拒否します。 |
| 所属グループの属性 | ユーザーエントリから読みます。AD は `memberOf` を標準で持ちますが、OpenLDAP は `memberof` オーバーレイが要ります。 |
| グループ検索のベース DN / フィルタ | オーバーレイの無いディレクトリ向けの任意の 2 回目の検索 — **かつ AD で入れ子グループを解決できる唯一の手段**です（AD の `memberOf` は推移的ではありません）。マッチングルール OID を使います: `(member:1.2.840.113556.1.4.1941:={user_dn})` |
| グループ → ロール | SSO プロバイダと同じ機構です。グループは完全な DN でも名前でも、大文字小文字を無視して一致し、複数一致したら最も高いロールが勝ちます。マッピングも既定ロールも無いと**全てのログインが拒否される**ため、その組み合わせを有効のまま保存することは拒否されます。 |

**テストボタンを使ってください。** *保存済み*の設定に対して実行し、段階ごとに結果を報告します —
ユーザー自身としては意図的にバインドしないので、緑になっても証明されるのは接続・TLS 信頼・
サービスアカウント・ロール解決までで、「そのパスワードが通るか」ではありません。ユーザー名を渡すと
DN・グループ・**そのユーザーが得るロール**も表示します。ここが拒否になるのが最頻の設定ミスで、
ログイン画面からはパスワード違いと区別が付きません。

**アカウントロックアウト。** Yagra は 5 回失敗でロックしますが、AD の `lockoutThreshold` も 5〜10 が
一般的です。つまり Yagra のログイン画面での打ち間違いが、Yagra だけでなく**ドメイン側のロックアウト**を
引き起こしえます。気になる場合は `yagra_ldap_bind_total` を監視してください。

ディレクトリをオフにすると、そこから作られた全アカウントのセッションが失効します。行自体は残るので、
再度オンにすれば復帰します。

## SAML

**Yagra は SAML を実装しません。これは欠落ではなく決定です**（ADR-041）。XML 署名検証
（canonicalization・XXE・署名ラッピング）は認証バイパスの常習的な発生源で、Rust の SP 実装は認証
経路を賭けられるほど成熟していません。

SAML しか話せない IdP には、**前段に SAML→OIDC ブリッジ**を置いてください。
[Keycloak](https://www.keycloak.org/) か [Dex](https://dexidp.io/) を OIDC プロバイダとして動かし、
SAML IdP と連携させたうえで、設定 ▸ 認証をそのブリッジに向けます。運用側の要件は満たせ、ブリッジは
それを専門とする人たちが保守し、Yagra の認証面はプロトコル 1 つ分小さいままで済みます。

---

## セキュリティ上の注意

- **信頼境界を越えるバスでは TLS が必須です。** ジョブメッセージはデバイス資格情報を平文で運びます — NATS `:4222` を平文でリモート拠点へ公開しないでください（**D · ステップ 1** 参照）。
- **KEK はマウントしたファイルであり、環境変数の値ではありません。** `YAGRA_KEK_FILE` で渡します。一時鍵フォールバックは開発専用です。
- **イメージは非 root で動きます**（core uid 10001、poller uid 10002 + `cap_net_raw+ep` ファイル capability、web nginx uid 101）。`NET_RAW` を得るのは poller だけです。
- **資格情報はログしません** — SNMP コミュニティ、SNMPv3 auth/priv、API トークンは保存時に暗号化され、ログ・API 応答・メトリクスラベルから伏せられます。
