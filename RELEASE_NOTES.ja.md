# リリースノート

## v0.1.0（未リリース）

リポジトリの初期スケルトンとテスト済みの中核。

### 新機能
- コンポーネントクレート（Yagra-core / Yagra-poller / Yagra-discovery / Yagra-alert /
  Yagra-bus / Yagra-transport / Yagra-topology）と共有ライブラリ（yagra-common /
  yagra-secrets）、`web/` 配下の Yagra-web WebUI からなる Cargo ワークスペース。
- テスト済みの中核ロジック: アラート品質（ヒステリシス / フラッピング / 依存抑制 /
  dedup・グルーピング / 通知ディスパッチ）、閾値継承、RBAC、エンベロープ暗号、
  Credential Finder のレート制限、バスメッセージ契約。
- 単一プロセスの「歩く骨格」（core → bus → poller → メトリクス → REST API）。
- React + TypeScript の WebUI（型付き API クライアント / SSE / ダッシュボード /
  アラート一覧 / グラフ）。
- 単一ノード MVP 向けの Docker Compose スケルトン（core / poller / ストア / バス / TSDB）。

> 機能リリースはまだありません。外部サービス（NATS / VictoriaMetrics / PostgreSQL /
> raw-socket ICMP / SNMP）との連携は実装中です。
