# ItalianSuperWhisper 🇮🇹

**Dettatura vocale per macOS, con l'italiano come lingua di riferimento.**

Premi una scorciatoia, parli, e il testo viene scritto nell'app che hai davanti.
Tutto avviene **sul tuo Mac**: nessuna chiave API, nessuna inferenza in rete. La rete
serve soltanto a scaricare i modelli la prima volta.

Fork di [Starmel/OpenSuperWhisper](https://github.com/Starmel/OpenSuperWhisper), che è
un'app multilingua generalista. Qui il baricentro è l'italiano.

---

## Focus sull'italiano

Non significa che le altre lingue siano state rimosse: Whisper e Parakeet continuano a
supportarle tutte e restano selezionabili. Significa che **le scelte di prodotto vengono
fatte guardando l'italiano**.

In concreto:

- **Gli anglicismi non sono errori.** In italiano parlato "meeting", "call", "deadline",
  "budget" sono normali. Non vengono tradotti né "corretti": vanno trascritti come li hai
  detti. Lo stesso vale per termini presi da altre lingue.
- **Niente logica specifica per lingue asiatiche.** L'autocorrect CJK dell'upstream
  (spaziatura fra caratteri cinesi/giapponesi e latini, punteggiatura full-width) è stato
  rimosso: non si applica all'italiano e trascinava l'intera toolchain Rust nel build.
- **Correzione automatica dell'italiano**, descritta qui sotto.

## Le due correzioni

Il testo che esce dal modello passa per due livelli, deliberatamente separati.

### 1. Correttore deterministico — attivo

`ItalianTextCorrector` gira su **ogni** dettatura. Non fa chiamate a modelli, quindi costa
microsecondi e non ha niente da configurare.

Contiene **solo regole sempre vere**:

- accenti la cui forma non accentata non è una parola italiana:
  `perche` → `perché`, `piu` → `più`, `puo` → `può`, `citta` → `città`
- grafie mai corrette: `pò` → `po'`, `qual'è` → `qual è`, `daccordo` → `d'accordo`,
  `un'altro` → `un altro`
- spazi prima della punteggiatura e spazi doppi

Tutto ciò che richiede contesto è **escluso di proposito**. `e`/`è`, `si`/`sì`, `la`/`là`,
`da`/`dà`, `ne`/`né`, `se`/`sé` hanno due grafie entrambe valide: una regola cieca
cambierebbe il significato della frase. Per lo stesso motivo sono fuori dalla tabella degli
accenti anche `pero` (l'albero), `meta` (l'obiettivo), `giacche` (i giubbotti), `te` (il
pronome) ed `eta` (la lettera greca).

Si attiva quando la lingua è impostata **esplicitamente** su italiano, non su "auto": con
"auto" la lingua non è nota e queste regole non devono girare su altre lingue.

### 2. Riformulazione con LLM locale — opzionale

Livello separato e disattivato di default, per ripulire le autocorrezioni del parlato. Da
*"domani alle 10 non ci sarò, ah no, non è vero, alle 10.30"* a
*"domani non ci sarò alle 10.30"*.

Whisper e Parakeet sono modelli di **trascrizione**: riportano fedelmente quello che hai
detto e non faranno mai questo lavoro. Serve un modello a parte, anch'esso in locale
(Gemma 4 E2B via MLX).

Si attiva da **Impostazioni → Transcription → Riformulazione**. Una volta attiva è
automatica: gira su ogni dettatura, senza nessun gesto in più. Da sapere prima di
accenderla:

- alla prima dettatura scarica un modello di alcuni GB;
- ogni dettatura richiede qualche secondo in più;
- il testo grezzo viene salvato nel database accanto a quello riscritto, e ogni errore
  del modello (mancato caricamento, risposta vuota, risposta fuori tema) fa ricadere
  sulla trascrizione originale. Una riformulazione fallita non ti costa la dettatura.

> **Regola valida per entrambi i livelli:** il testo grezzo viene sempre conservato accanto
> a quello corretto. Alterare in silenzio quello che hai dettato è peggio che non fare
> nulla.

## Funzionalità

- Registrazione e trascrizione in locale
- Due motori: [Whisper](https://github.com/ggerganov/whisper.cpp) e
  [Parakeet](https://github.com/AntinomyCollective/FluidAudio), con download dei modelli
  dall'app
- Scorciatoia globale — combinazione di tasti oppure singolo modificatore (⌘ sinistro,
  ⌥ destro, Fn…)
- Trigger da mouse — tasto centrale o tasti laterali del pollice
- Modalità premi-e-tieni: tieni premuto per registrare, rilascia per fermare
- Trascina e rilascia file audio, con coda di elaborazione
- Scelta del microfono: integrato, esterno, Bluetooth, iPhone (Continuity)
- Più lingue con rilevamento automatico — l'italiano è il focus, non un vincolo

## Requisiti

- macOS 14 o successivo
- Mac Apple Silicon (ARM64)

## Come farlo girare

### 1. Xcode completo

Serve **Xcode**, non solo i Command Line Tools: il build usa `xcodebuild` sul progetto
`OpenSuperWhisper.xcodeproj`. Verifica con `xcodebuild -version`; se risponde che serve
Xcode, indirizza la toolchain:

```shell
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

### 2. Dipendenze di build

```shell
brew install cmake libomp ruby
gem install xcpretty
```

`xcpretty` è opzionale: serve solo a rendere leggibile l'output del build.

### 3. Clona e compila

```shell
git clone git@github.com:dylanpatriarchi/ItalianSuperWhisper.git
cd ItalianSuperWhisper
git submodule update --init --recursive
./run.sh build
```

Il passo dei submodule **non è opzionale**: senza, `libwhisper/whisper.cpp` resta vuoto e
il link fallisce con `library 'ggml-metal' not found`.

### 4. Firma l'app

Il build produce un'app non firmata, e macOS non ti lascia concedere i permessi di
Accessibilità (necessari per la scorciatoia globale e per incollare il testo) finché non ha
una firma. Firmala ad-hoc:

```shell
codesign --force --sign - \
  --entitlements OpenSuperWhisper/OpenSuperWhisper.entitlements \
  build/Build/Products/Debug/OpenSuperWhisper.app
```

### 5. Avvia

```shell
open build/Build/Products/Debug/OpenSuperWhisper.app
```

Al primo avvio dovrai concedere **Accessibilità** e **Microfono** in Impostazioni di
Sistema, e scaricare un modello dalle impostazioni dell'app.

In caso di problemi, `.github/workflows/build.yml` è il workflow di CI e mostra la
sequenza esatta che gira automaticamente a ogni push.

## Modelli

Per l'italiano serve un modello **multilingue**: quelli con suffisso `.en`
(`tiny.en`, `base.en`…) sono solo inglese.

Dalle impostazioni dell'app:

- **Turbo V3 large** (~1,6 GB) — default ragionevole, buona resa in italiano
- **Parakeet v3** — più leggero e veloce, supporta anch'esso l'italiano

Entrambi i motori sono nell'app: puoi confrontarli sulla tua voce e tenere quello che ti
rende meglio.

I modelli Whisper si possono anche scaricare a mano dal
[repository Hugging Face di whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp/tree/main)
e mettere nella cartella dei modelli dell'app.

## Correzioni rispetto all'upstream

Applicate in [#1](https://github.com/dylanpatriarchi/ItalianSuperWhisper/pull/1):

- **L'annullamento di una trascrizione ora funziona.** Il flag di cancellazione veniva
  impostato e subito riazzerato nella stessa chiamata, e il motore Parakeet non salvava mai
  il proprio task: annullare lasciava il lavoro in esecuzione fino alla fine.
- **Due trascrizioni non possono più entrare nello stesso contesto whisper in parallelo.**
- **La callback di progresso non può più sopravvivere all'oggetto che punta**, che poteva
  liberare memoria ancora referenziata da whisper.cpp.
- **I modelli scaricati vengono validati.** Prima si controllava solo lo stato HTTP: una
  pagina di errore poteva essere salvata come modello e risultare installata per sempre.
- **Due dettature ravvicinate non distruggono più la clipboard**, che veniva ripristinata
  alla *trascrizione precedente* invece che al contenuto dell'utente.
- **Registrare prima che il modello sia carico ora lo dice**, invece di scartare la
  dettatura in silenzio.
- Gli event tap non perdono più una mach port a ogni cambio di scorciatoia.
- `run.sh` non riporta più successo quando il build è fallito.

## Roadmap

- [x] Rimozione dell'autocorrect CJK e della toolchain Rust dal build
- [x] Correttore italiano deterministico a bassa latenza
- [x] Riformulazione con LLM locale, opzionale e automatica una volta attiva
- [x] Conservazione del testo grezzo nel database, accanto a quello corretto
- [ ] Confronto Whisper vs Parakeet sull'italiano per scegliere il default

Ereditati dall'upstream:

- [ ] Trascrizione in streaming
- [ ] Dizionario personalizzato / keyword boosting
- [ ] Compatibilità con Mac Intel
- [ ] Modalità agente

## Contribuire

Pull request e issue sono benvenute.

## Licenza

MIT. Vedi il file [LICENSE](LICENSE).
