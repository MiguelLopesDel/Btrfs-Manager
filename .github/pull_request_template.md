## Objetivo

Descreva o que este PR muda e por quê.

## Risco principal

Explique o maior risco técnico ou de produto desta mudança.

## Testes executados

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --no-default-features -- -D warnings`
- [ ] `cargo test --workspace --all-targets --no-default-features`
- [ ] `python3 scripts/quality-gate.py check --write-report`
- [ ] `bash scripts/dev-loopback-btrfs-test.sh`
- [ ] `bash scripts/e2e-headless-smoke.sh` quando tocar na GUI
- [ ] Teste GUI manual, quando aplicável

## Evidência de regressão/TDD

- [ ] A mudança inclui teste novo ou ajustado para o comportamento alterado
- [ ] Bug fix inclui teste que reproduz o bug
- [ ] Sem teste novo porque:

## Impacto em segurança e dados

- [ ] Não altera comandos privilegiados
- [ ] Não altera validação de paths
- [ ] Não altera snapshots, rollback ou dados do usuário
- [ ] Altera uma área acima e explica o motivo abaixo

## Review de arquitetura

Descreva impactos na fronteira GUI/helper, reversibilidade, acoplamento entre crates e comportamento em caso de erro.

## Evidências visuais

Inclua screenshots ou gravações quando tocar na GUI.
