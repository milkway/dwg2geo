# dwg2geo — início rápido

Este pacote contém um plano técnico, prompts para Codex CLI e Claude Code e um esqueleto Rust para uma CLI de conversão DWG -> GeoJSON.

## O que já está desenhado

- `inspect`: identifica a assinatura DWG, geração do AutoCAD, tamanho e SHA-256 sem depender de bibliotecas CAD.
- `doctor`: verifica `dwgread` e `ogr2ogr`.
- `convert`: usa LibreDWG + GDAL e exige o CRS de origem, salvo quando coordenadas locais são aceitas explicitamente.
- backend nativo futuro com `acadrust`, isolado por feature do Cargo.
- plano por marcos, arquitetura, mapeamento de entidades, riscos e decisões.

O desenho `_Corredor Sul.dwg` não foi incluído no ZIP. Apenas sua assinatura, tamanho e hash foram registrados em `samples/corredor-sul.metadata.json`.

## Como começar

```bash
unzip dwg2geo-starter-pack.zip
cd dwg2geo-starter
cargo fmt --check
cargo check
cargo test
```

Copie o DWG localmente:

```bash
cp "/caminho/_Corredor Sul.dwg" samples/
```

Inspecione:

```bash
cargo run -- inspect "samples/_Corredor Sul.dwg" --json
```

Inicie o agente:

```bash
codex "$(cat prompts/START_CODEX.md)"
```

ou:

```bash
claude "$(cat prompts/START_CLAUDE.md)"
```

O arquivo `AGENTS.md` contém as regras canônicas do projeto. O `PROMPT.md` é uma versão única que funciona como ponto de partida para qualquer agente de terminal.
