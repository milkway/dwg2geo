# dwg2geo (português)

> Versão resumida — a documentação completa e sempre atual está no [README em inglês](README.md).

CLI e biblioteca de código aberto que convertem desenhos DWG de engenharia em **GeoJSON auditável**, com tratamento explícito de sistema de referência de coordenadas (CRS), diagnósticos e relatórios rastreáveis.

**Site e documentação:** <https://milkway.github.io/dwg2geo/>

## Instalação

Binários prontos (Linux, macOS, Windows) na [página de releases](https://github.com/milkway/dwg2geo/releases/latest), ou pelos registros de pacotes:

| Ecossistema | Instalação | Uso |
|---|---|---|
| Rust ([crates.io](https://crates.io/crates/dwg2geo)) | `cargo add dwg2geo --features native-backend` (CLI: `cargo install dwg2geo`) | `dwg2geo::backend::native::convert_bytes(...)` |
| JavaScript/WASM ([npm](https://www.npmjs.com/package/dwg2geo)) | `npm install dwg2geo` | `import init, { convert } from 'dwg2geo'` — roda no navegador |
| Python ([PyPI](https://pypi.org/project/dwg2geo/)) | `pip install dwg2geo` | `dwg2geo.convert_file("desenho.dwg")` → dict com GeoJSON e relatório |

Experimente no navegador, sem instalar nada: **<https://milkway.github.io/dwg2geo-app/>** (o arquivo nunca sai da sua máquina).

## Uso básico

```bash
# inspecionar um DWG (assinatura, versão, entidades, layers)
dwg2geo inspect desenho.dwg --json

# converter com CRS de origem conhecido (reprojetado para WGS 84)
dwg2geo convert desenho.dwg --backend native \
  --source-crs EPSG:31983 --source-units m -o saida.geojson

# ou exportar coordenadas locais explicitamente
dwg2geo convert desenho.dwg --backend native \
  --allow-local-coordinates -o saida-local.geojson
```

Cada conversão grava um relatório auditável em `<saída>.report.json` (opções, versões, hash SHA-256 da origem, contagens por tipo de entidade com motivos de descarte, avisos). A conversão **nunca adivinha o CRS**: sem `--source-crs`, só prossegue com `--allow-local-coordinates` explícito.

O desenho de referência e todos os dados derivados dele ficam fora deste repositório — o diretório `samples/` inteiro é ignorado pelo git.

## Licença

MIT (`LICENSE-MIT`). Detalhes de arquitetura, backends, calibração por pontos de controle e limitações: [README em inglês](README.md).
