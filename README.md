# convt (fork Windows)

Conversão local de imagens, vídeos, áudio, PDFs e documentos, pelo aplicativo ou pela linha de comando. Os arquivos são processados na sua máquina; as conversões locais não enviam seu conteúdo para um servidor.

Este é o fork [buddhadaiki/convt](https://github.com/buddhadaiki/convt), com foco no instalador e na integração com o Windows x64. O projeto original é [opencoredev/convt](https://github.com/opencoredev/convt), criado por [Leo (@leodev)](https://x.com/leodev). O aplicativo usa Rust e [GPUI](https://www.gpui.rs); o mesmo registro de motores atende ao app e à CLI.

## Versão experimental para Windows x64

A versão experimental **0.5.1** deste fork parte do upstream 0.5.0. Consulte os arquivos na [release 0.5.1 do fork](https://github.com/buddhadaiki/convt/releases/tag/v0.5.1):

- [Instalador MSI](https://github.com/buddhadaiki/convt/releases/download/v0.5.1/convt-0.5.1-windows-x86_64.msi): aplicativo, CLI e integração do Explorer, instalados por usuário.
- [SHA-256 do MSI](https://github.com/buddhadaiki/convt/releases/download/v0.5.1/convt-0.5.1-windows-x86_64.msi.sha256): checksum do instalador.
- [Pacote de documentos](https://github.com/buddhadaiki/convt/releases/download/v0.5.1/convt-0.5.1-windows-x86_64-documents.tar.gz) e seu [SHA-256](https://github.com/buddhadaiki/convt/releases/download/v0.5.1/convt-0.5.1-windows-x86_64-documents.tar.gz.sha256): suporte opcional, separado do MSI.

**O instalador é experimental e não tem assinatura Authenticode.** A coleta completa do código-fonte correspondente às dependências nativas FFmpeg/PDFium permanece pendente; [`inputs.lock.json`](packaging/windows/inputs.lock.json) mantém `distribution_ready=false`. A compilação de verificação não encerra essa pendência nem comprova que a distribuição está pronta. A preparação desta versão também não representa uma nova validação visual do aplicativo ou do menu do Explorer. Veja os limites e o histórico no [guia de empacotamento Windows](packaging/windows/README.md).

A suíte Rust registra uma falha conhecida no teste sintético de orientação HEIC com transparência, reproduzida também com as DLLs anteriores. O relatório de validação da release preserva essa falha; ela não comprova um defeito em HEIC reais nem uma regressão desta versão.

Baixe o MSI e seu checksum da mesma release. No PowerShell, dentro da pasta de download:

```powershell
$arquivo = '.\convt-0.5.1-windows-x86_64.msi'
$esperado = ((Get-Content -LiteralPath "$arquivo.sha256" -Raw).Trim() -split '\s+')[0]
$calculado = (Get-FileHash -LiteralPath $arquivo -Algorithm SHA256).Hash
if ($calculado -ne $esperado) { throw 'SHA-256 diferente: não execute o instalador.' }
```

Com os hashes iguais, abra o MSI. A instalação oferece opções de menu de contexto e atalho; as configurações e o histórico do aplicativo são preservados na desinstalação.

## Explorer e Quick convert

Clique com o botão direito em um arquivo e escolha **Convert with convt**. O menu clássico também fica em **Mostrar mais opções** no Windows 11. O menu principal do Windows 11 depende do **Modo de Desenvolvedor já habilitado** e da validação da identidade Appx e da ativação COM; o instalador não altera essa configuração, políticas ou confiança em certificados.

Os destinos rápidos seguem **Settings → Menu** ou `convt menu`. **Mais** abre Quick convert com os arquivos selecionados e os destinos compatíveis, aguardando sua escolha e o comando de conversão. As preferências do menu não limitam as conversões disponíveis na CLI ou em Quick convert. Consulte os detalhes da [integração com o Explorer](integrations/windows/README.md).

## Alterações mantidas neste fork

- Recuperação transacional da integração do instalador e preservação dos dados de recuperação quando a limpeza falha. O histórico registra uma falha do Windows Installer ao restaurar seus próprios metadados; o rollback completo do MSI permanece sem comprovação nesse cenário.
- Supervisão dos auxiliares do instalador por Job Object, encerrando e aguardando os processos descendentes antes da compensação em cancelamento ou timeout.
- Downloads de empacotamento com espelhos e verificação SHA-256, recusando arquivos incorretos e parciais, com testes de regressão para espelhos e integridade do cache.
- Leitura de MPEG (`.mpg`, `.mpeg`, incluindo vídeo elementar) e `.mpg4`, com regressões de conversão. Esses formatos são entradas, não destinos de saída.

## Uso pela CLI

O MSI não adiciona a CLI ao `PATH`. No PowerShell, use o executável na pasta escolhida na instalação. Para a pasta padrão:

```powershell
$convt = Join-Path $env:LOCALAPPDATA 'Programs\convt\convt.exe'
& $convt photo.png --to webp
& $convt clip.mpg --to mp4
& $convt photos/ --to webp -r
& $convt lease.pdf --to png --pages 1-3
& $convt engines
& $convt targets photo.heic
& $convt targets photo.heic --menu
& $convt formats
& $convt presets
& $convt menu show
& $convt pack status documents
```

O resultado é salvo ao lado da entrada, salvo uso de `--out-dir`. `convt --help` lista as opções de qualidade, tamanho, páginas, DPI, vídeo e paralelismo. Os destinos dependem dos motores disponíveis: FFmpeg, PDFium, LibreOffice, image, resvg e libheif.

O suporte a Word, Excel e PowerPoint usa LibreOffice do sistema ou o pacote opcional de documentos. Quando a release oferecer esse pacote, `convt pack install documents` baixa o arquivo configurado na compilação e verifica seu SHA-256 antes de instalar. Ele não faz parte do MSI e não é baixado automaticamente pelos motores. Compilações de fonte podem usar LibreOffice local ou uma origem explícita com `--source` e `--sha256`.

## Compilar a partir do código

No Windows, veja os pré-requisitos e o script de preparação em [`scripts/setup-windows.ps1`](scripts/setup-windows.ps1). Esse script instala ferramentas e dependências de desenvolvimento. Com o ambiente preparado:

```powershell
cargo run -p convt-cli -- photo.png --to webp
cargo build --release -p convt-cli -p convt-app -p convt-shell
```

O MSI exige também os motores nativos, os recursos de identidade e o empacotamento descritos no [guia Windows](packaging/windows/README.md). Build, testes sem interface e validação do aplicativo aberto são verificações distintas.

- [AGENTS.md](AGENTS.md): estrutura do repositório, convenções e validação.
- [Integração Windows](integrations/windows/README.md): menus, configuração e probes.
- [Guia de releases](docs/release.md): artefatos, fontes correspondentes e verificações de distribuição.
- [Inventário de licenças](docs/licence-inventory.md): dependências e pendências de distribuição.

## Créditos e licença

Código sob [GNU AGPL-3.0-only](LICENSE), preservando os créditos do projeto original e os avisos das dependências. O nome **convt** e seu logotipo são marcas e não estão cobertos pela licença do código.

Este fork mantém os mecanismos existentes de avaliação e licença. As políticas comerciais, os planos e os serviços oficiais pertencem ao projeto original; consulte [convt.app](https://convt.app) e a [documentação oficial](https://convt.app/docs). A API cloud é um serviço separado que recebe os arquivos enviados pelo cliente.
