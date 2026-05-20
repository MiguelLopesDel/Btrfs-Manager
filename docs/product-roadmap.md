# Btrfs Manager Product Roadmap

Este documento é a fonte de verdade do produto. Ele não deve ser usado como
lista otimista de código escrito; uma tarefa só é concluída quando está pronta
para uso real.

## Objetivo do Produto

Btrfs Manager é um utilitário desktop moderno para Linux que gerencia
subvolumes, snapshots, navegação somente leitura, comparação, agendamento,
retenção e rollback reversível em sistemas Btrfs.

O alvo de v1 são usuários técnicos de Linux desktop que entendem Btrfs, mas
querem uma ferramenta gráfica clara, segura e menos confusa que as alternativas
atuais.

## Escopo Realista

Este projeto não deve tentar virar Timeshift, Snapper e Btrfs Assistant ao
mesmo tempo no curto prazo. A complexidade real não está em chamar
`btrfs subvolume snapshot`; está em suportar layouts diferentes de distro,
permissões, Polkit, LUKS, bootloaders, rollback reversível, cleanup de mounts,
retenção, UI segura e testes em filesystem real.

O escopo viável é evoluir por camadas:

1. **v0.1 System foundation**: arquitetura de privilégio correta, discovery,
   inventário e browse read-only confiáveis.
2. **v0.2 Managed snapshots**: criar/remover snapshots gerenciados com estado
   persistido e UI segura.
3. **v0.3 Timeline and retention**: timeline, filtros, políticas e retenção
   bem testadas.
4. **v0.4 Compare and restore files**: comparação e restore parcial sem
   rollback de sistema.
5. **v0.5 Reversible rollback**: rollback de root apenas depois de validação em
   VM, bootloader e transação reversível.

## Stack-Alvo

- Rust workspace.
- GTK4/libadwaita para a aplicação desktop.
- `crates/core` para modelos, parsing, retenção, comparação e planos de
  rollback.
- Serviço privilegiado D-Bus no system bus para operações reais.
- Polkit para autorização por classe de ação.
- CLI administrativa para debug, testes e automação, mas não como caminho
  primário da GUI.
- Arch/AUR como primeiro alvo de empacotamento.
- UI em português ou inglês de forma consistente por build, com estrutura i18n.

## Princípios Arquiteturais

- A GUI roda sem privilégio.
- A GUI não executa `btrfs`, `mount`, `umount`, `systemctl`, `pkexec` ou o
  helper CLI diretamente no caminho de produção.
- Operações reais da GUI passam pelo serviço D-Bus `org.btrfsmanager.Helper`.
- O serviço D-Bus roda como root e autoriza métodos por Polkit.
- `pkexec` não é fallback de produção. Se o serviço não estiver instalado, a
  GUI deve mostrar erro claro de instalação/configuração.
- O helper CLI continua existindo para administração, testes e desenvolvimento.
- Snapshots criados pelo app são read-only por padrão.
- Snapshots externos de Snapper/Timeshift/grub-btrfs/refind-btrfs são
  detectados, mas não modificados por padrão.
- Operações destrutivas exigem confirmação explícita.
- Rollback de root precisa ser reversível: antes de ativar rollback, preservar
  o estado atual como snapshot de retorno.
- Testes normais usam imagem Btrfs loopback descartável; boot/rollback usam VM.

## Definition of Done

Uma tarefa só pode ser marcada como concluída quando todos os itens abaixo forem
verdadeiros:

- Código implementado no caminho arquitetural correto.
- Testes unitários ou de integração cobrem o comportamento novo.
- Regressão conhecida tem teste que falharia antes da correção.
- `cargo fmt`, `clippy`, testes e quality gates passam.
- Fluxo validado em loopback ou host real, conforme o tipo da feature.
- Para GUI: fluxo testado visualmente e erro apresentado de forma clara.
- Para privilégio: comportamento validado via D-Bus/Polkit instalado, não só via
  fallback local.
- Para empacotamento: arquivos instalados no local esperado e serviço funciona
  após instalação.
- Documentação/roadmap atualizados.

Estados permitidos:

- `[ ]` Não iniciado.
- `[~]` Em protótipo: existe código, mas ainda falta validação real.
- `[x]` Concluído de acordo com a Definition of Done.

## Estado Atual

O projeto já tem um protótipo funcional amplo, mas nem tudo deve ser tratado
como pronto.

Concluído:

- [x] Workspace Rust com crates `core`, `helper` e `app`.
- [x] Modelos básicos para filesystem, subvolumes e snapshots.
- [x] Parser de `btrfs subvolume list -u` com testes.
- [x] Classificação de subvolume normal, container, snapshot e snapshot externo
  com testes.
- [x] Quality gates iniciais no repositório e GitHub Actions.
- [x] Script loopback Btrfs com falha segura quando loop device não existe.

Em protótipo:

- [~] Serviço D-Bus e ações Polkit.
- [~] GUI GTK/libadwaita.
- [~] Discovery de filesystems reais.
- [~] Browse read-only de snapshots.
- [~] Cleanup de mounts temporários.
- [~] Agendamento/retenção via systemd timer.
- [~] Documentação técnica HTML.

Não iniciado de forma pronta:

- [ ] UI de criação manual de snapshot.
- [ ] Timeline avançada.
- [ ] Comparação na GUI.
- [ ] Restore parcial.
- [ ] Rollback reversível de root.
- [ ] Pacote instalável validado em máquina limpa.

## Fase 0: Fundação De Qualidade

Objetivo: impedir degradação antes de crescer o escopo.

Tarefas:

- [x] CI com fmt, clippy, testes, docs, shellcheck, cargo-deny e CodeQL.
- [x] Quality ratchet com baseline de complexidade, duplicação e cobertura.
- [x] Workflow de mutation testing manual/agendado.
- [x] PR template focado em risco, testes e arquitetura.
- [x] Branch protection configurada no GitHub.

Aceite:

- [x] `main` protegida com required checks.
- [x] `.env`, artefatos locais e skills globais não entram no Git.
- [x] Quality gates rodam localmente e no CI.

## Fase 1: Boundary De Privilégio D-Bus/Polkit

Objetivo: limpar a arquitetura antes de novas features. Esta fase bloqueia o
resto do roadmap.

Tarefas:

- [x] Manter helper CLI como interface administrativa/dev.
- [x] Manter serviço D-Bus como única interface da GUI para operações reais.
- [x] Remover `pkexec` do caminho normal da GUI.
- [x] Remover execução direta de helper local pela GUI para build instalado.
- [x] Adicionar modo dev explícito para fallback local, ativado por variável de
  ambiente com nome claro.
- [x] Se o serviço D-Bus não existir, a GUI mostra erro acionável de instalação,
  não tenta caminhos mágicos.
- [x] Separar contrato D-Bus do contrato CLI em documentação.
- [x] Criar teste que falha se `crates/app` invocar `pkexec`, `btrfs`, `mount`,
  `umount` ou `systemctl`.
- [x] Criar script de validação de instalação D-Bus/Polkit para host real.
- [ ] Testar serviço instalado no system bus com Polkit em host real.

Aceite:

- [x] Abrir a GUI instalada não chama `pkexec`.
- [ ] Discovery/listagem funcionam via D-Bus/Polkit no host real.
- [ ] Operações privilegiadas pedem autenticação apenas na ação correta.
- [x] Se o serviço estiver ausente, o erro da GUI explica como instalar/iniciar.
- [x] Testes e quality gates passam localmente.

## Fase 2: Discovery E Inventário Confiáveis

Objetivo: a aplicação entende o sistema Btrfs antes de permitir mutações.

Tarefas:

- [~] Descobrir filesystems Btrfs com UUID, devices, mountpoints e subvolume
  ativo.
- [~] Resolver paths quando `/` está montado como `subvol=@`.
- [x] Distinguir subvolumes, containers, snapshots reais e snapshots externos.
- [~] Detectar Snapper, Timeshift, grub-btrfs e refind-btrfs de forma
  conservadora.
- [~] UI com selector de filesystem sem mountpoint hardcoded.
- [ ] Erros de discovery padronizados e traduzíveis.
- [ ] Teste de host real documentado com output esperado.

Aceite:

- [ ] No loopback, lista subvolumes e snapshots corretamente.
- [ ] No host root Btrfs, lista subvolumes sem erro de permissão na GUI.
- [ ] `@snapshots` aparece como container, não como snapshot.
- [ ] Paths usados em browse/mount são válidos.
- [ ] Testes e quality gates passam.

## Fase 3: Browse Read-Only De Snapshots

Objetivo: navegar snapshots com segurança e cleanup previsível.

Tarefas:

- [~] Montar snapshot como read-only em path gerenciado.
- [~] Abrir snapshot no file manager.
- [~] Mostrar estado montado na UI.
- [~] Ação explícita de unmount.
- [~] Cleanup de mounts da sessão ao fechar a GUI.
- [ ] Cleanup global seguro via serviço D-Bus.
- [ ] Nome curto e legível para diretórios temporários de browse.
- [ ] Teste que garante escrita negada no mount browse.
- [ ] Teste de cleanup no loopback.

Aceite:

- [ ] Clique em browse abre view read-only.
- [ ] Escrita no browse path falha.
- [ ] Fechar GUI desmonta mounts criados por aquela sessão.
- [ ] `lsblk` não fica poluído após cleanup.
- [ ] Testes e quality gates passam.

## Fase 4: Snapshots Gerenciados Manuais

Objetivo: criar e remover snapshots do app com estado persistido.

Tarefas:

- [ ] UI de criar snapshot para subvolume selecionado.
- [ ] Convenção de destino sob snapshot root configurável.
- [ ] Nome gerado previsível com timestamp.
- [ ] Tags/notas opcionais.
- [ ] Persistir metadata SQLite.
- [ ] Refresh de inventário após criação/remoção.
- [ ] Delete apenas para snapshots gerenciados, salvo opt-in explícito.

Aceite:

- [ ] Usuário cria snapshot read-only pela UI.
- [ ] Snapshot aparece como gerenciado.
- [ ] Metadata sobrevive a reinício da GUI.
- [ ] Delete exige confirmação e remove só alvo correto.
- [ ] Testes e quality gates passam.

## Fase 5: Timeline, Busca E Filtros

Objetivo: listas grandes ficam navegáveis.

Tarefas:

- [ ] Agrupar timeline por dia/mês.
- [ ] Filtrar por filesystem, subvolume, managed/external, readonly/unlocked,
  tags e data.
- [ ] Buscar por path, tag, source e nota.
- [ ] Ordenar por criação, path e source.
- [ ] Empty states objetivos.

Aceite:

- [ ] Filtros não reexecutam comandos Btrfs.
- [ ] Lista grande continua legível.
- [ ] Testes e quality gates passam.

## Fase 6: Retenção E Agendamento

Objetivo: snapshots automáticos sem daemon próprio sempre rodando.

Tarefas:

- [~] Políticas com presets hourly/daily/weekly/monthly.
- [~] Geração de systemd timers.
- [~] Preview de retenção.
- [~] Logs de execução.
- [ ] UI revisada e validada em host systemd real.
- [ ] Nunca remover snapshots externos ou rollback anchors.
- [ ] Teste de policy run em ambiente controlado.

Aceite:

- [ ] Timer cria snapshot sem GUI aberta.
- [ ] Retenção remove apenas snapshots gerenciados elegíveis.
- [ ] Logs aparecem na UI.
- [ ] Testes e quality gates passam.

## Fase 7: Unlock/Lock Avançado

Objetivo: permitir edição de snapshots de forma explícita e auditável.

Tarefas:

- [~] Ação para `ro=false`.
- [~] Ação para `ro=true`.
- [ ] Confirmação forte explicando riscos de snapshot gravável.
- [ ] Marcar snapshot como dirty/unlocked no estado.
- [ ] Bloquear snapshots externos por padrão.

Aceite:

- [ ] Snapshot gerenciado pode ser desbloqueado e bloqueado novamente.
- [ ] UI marca estado dirty/unlocked.
- [ ] Externos continuam protegidos.
- [ ] Testes e quality gates passam.

## Fase 8: Comparação E Restore Parcial

Objetivo: comparar e restaurar arquivos sem mexer no boot.

Tarefas:

- [~] Primitivo de comparação por path, tipo, size e mtime.
- [ ] UI para comparar snapshot com source/outro snapshot.
- [ ] Comparação escopada por pasta.
- [ ] Diff textual para arquivos pequenos.
- [ ] Restore parcial com confirmação e preview.

Aceite:

- [ ] Comparação funciona no loopback.
- [ ] Operações longas podem ser canceladas ou limitadas.
- [ ] Restore parcial preserva arquivo existente conforme política escolhida.
- [ ] Testes e quality gates passam.

## Fase 9: Rollback Reversível

Objetivo: rollback de root seguro, conservador e testado em VM.

Tarefas:

- [ ] Staging de rollback a partir de snapshot selecionado.
- [~] Modelo para snapshot de retorno do estado atual.
- [ ] Persistir transação SQLite.
- [ ] Integração grub-btrfs quando disponível.
- [ ] Integração refind-btrfs quando disponível.
- [ ] Instruções manuais para bootloaders não suportados.
- [ ] Detectar pós-reboot e oferecer commit/revert.

Aceite:

- [ ] Funciona em VM limpa com layout documentado.
- [ ] Estado atual é preservado como retorno.
- [ ] App mostra pending/activated/reverted.
- [ ] Testes e quality gates passam.

## Fase 10: Empacotamento E Release

Objetivo: instalar em máquina limpa e funcionar fora do repositório.

Tarefas:

- [~] Arch PKGBUILD.
- [~] Desktop file.
- [~] Polkit policy.
- [~] D-Bus service.
- [~] systemd units.
- [ ] Ícone.
- [ ] Traduções instaladas.
- [ ] Teste em Arch limpo.
- [ ] Publicação AUR opcional.

Aceite:

- [ ] Fresh Arch build instala app, helper, D-Bus, Polkit e desktop entry.
- [ ] App abre pelo launcher.
- [ ] Serviço D-Bus responde.
- [ ] Polkit autoriza conforme esperado.
- [ ] Testes e quality gates passam.

## Direção De UI

O app deve parecer um utilitário de sistema silencioso e confiável.

Tela principal:

- Header com título e ações globais.
- Timeline/lista de snapshots como foco.
- Busca sempre visível.
- Filtros compactos.
- Selector de filesystem discreto.
- Rows legíveis com ações: browse, compare, rollback, more.
- Subvolumes visíveis, mas secundários em relação a snapshots.

Evitar:

- Textos explicativos longos dentro do app.
- Mountpoints de laboratório em UI normal.
- Ações destrutivas em row principal antes do fluxo seguro existir.
- Misturar português e inglês no mesmo build sem motivo.

## Estratégia De Testes

Camadas:

- Unit tests para parser, retenção, path safety e comparação.
- Testes de contrato para impedir que a GUI execute comandos privilegiados.
- Script loopback Btrfs para comportamento real de subvolume/snapshot/mount.
- Smoke E2E headless para a GUI iniciar.
- VM apenas para root rollback e bootloader.

Comandos recorrentes:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo clippy -p btrfs-manager-app --features gui --all-targets -- -D warnings
cargo test --workspace --all-targets --no-default-features
python3 scripts/quality-gate.py check --write-report
bash scripts/e2e-headless-smoke.sh
bash scripts/dev-loopback-btrfs-test.sh
```

## Próximo Segmento Bloqueante

O próximo trabalho é a **Fase 1: Boundary De Privilégio D-Bus/Polkit**.

Não devemos avançar para criar snapshots pela UI, retenção nova ou rollback até
essa fase estar realmente pronta em host instalado:

1. GUI sem `pkexec`.
2. GUI sem execução direta de helper local no modo instalado.
3. Serviço D-Bus instalado e validado.
4. Polkit com prompts previsíveis.
5. Teste impedindo regressão arquitetural.
6. Documentação de instalação/debug.
