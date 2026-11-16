# wshare

Condivisione dello schermo nella rete locale, per Windows: un PC (l'**host**)
mostra il suo schermo e si lascia controllare; un altro (il **visualizzatore**)
lo guarda e lo usa. Video H.264/HEVC su QUIC, con audio, appunti condivisi e
trasferimento di file.

- **Bassa latenza**: cattura (DXGI Desktop Duplication), conversione colore e
  codifica hardware (Media Foundation) restano sulla GPU; il bitrate si adatta
  alla congestione della rete.
- **Niente server**: i PC si trovano da soli nella LAN (mDNS) e si parlano
  direttamente, cifrati con TLS 1.3 (QUIC).
- **Controllo remoto** di mouse e tastiera, **audio** del PC condiviso,
  **appunti** condivisi (testo e file copiati), **file** trascinati o inviati
  in entrambe le direzioni, **Wake-on-LAN** dei PC conosciuti.
- **Accesso non presidiato** facoltativo: schermata di blocco, accesso a
  Windows, UAC e Ctrl+Alt+Canc (vedi sotto).
- **Aggiornamento automatico** dalle GitHub Release, con verifica della firma.

## Installazione

Requisiti: Windows 10 o 11 a 64 bit, con una scheda video che codifichi
H.264 (praticamente tutte quelle degli ultimi dieci anni).

1. Scarica `wshare-<versione>-windows-x64.zip` dall'[ultima
   release](https://github.com/necos98/wshare/releases/latest).
2. Estrai `wshare.exe` e `wshare-cli.exe` in una cartella **tua** (ad esempio
   `%LOCALAPPDATA%\Programs\wshare`): l'aggiornamento automatico deve poter
   scrivere lì, quindi non in `Program Files`.
3. Avvia `wshare.exe`. Al primo avvio Windows chiede di consentirlo nel
   firewall: consenti le **reti private** (porta UDP 7420 per le connessioni,
   UDP 5353 per trovare i PC).

Non serve un'installazione: è tutto in quei due file. I dati (identità TLS,
dispositivi autorizzati, impostazioni) stanno in `%LOCALAPPDATA%\wshare`.

## Uso

1. Sul PC da condividere: attiva «Condividi questo schermo»; la finestra
   mostra un **PIN**.
2. Sull'altro PC: scegli il computer dall'elenco (o scrivi il suo indirizzo)
   e inserisci il PIN.

Un visualizzatore nuovo entra con il PIN mostrato sull'host (o con un clic di
chi è davanti all'host, se l'impostazione «Accesso» lo consente); da quel
momento il suo dispositivo è **autorizzato**, riconosciuto dal certificato
TLS, e si connette senza PIN. Nella finestra del visualizzatore
Ctrl+Alt+Invio passa a schermo intero e Ctrl+Alt+I mostra le statistiche.

Tutto si può fare anche dal terminale con `wshare-cli` (che aspetta la fine
del comando e ne restituisce il codice di uscita):

```
wshare-cli help                        # tutti i comandi
wshare-cli host [--pin 123456]         # condivide questo schermo
wshare-cli list                        # i PC trovati nella LAN
wshare-cli view 192.168.1.20 --pin 123456
wshare-cli update --check              # c'è una versione nuova?
```

## Aggiornamenti

All'avvio wshare controlla se su GitHub c'è una versione più recente (si può
disattivare in Impostazioni → «Aggiornamenti»). Se c'è, la finestra principale
mostra «Aggiornamento disponibile» e le Impostazioni il pulsante **«Aggiorna e
riavvia»**, che:

1. scarica `latest.json` e la sua firma `latest.json.sig` dall'ultima release
   e verifica la firma **Ed25519** con la chiave pubblica compilata in wshare
   (`crates/release/update-key.pub`): un manifest non firmato o alterato viene
   ignorato;
2. scarica `wshare.exe` e `wshare-cli.exe` accanto a quelli in uso (come
   `*.new`) e ne controlla dimensione e SHA-256 con quelli del manifest
   firmato;
3. li sostituisce: quelli in uso diventano `*.old` (Windows permette di
   rinominare un programma in esecuzione, non di sovrascriverlo) e se un
   passaggio fallisce rimette tutto com'era;
4. interrompe la condivisione e riapre wshare con la nuova versione; i file
   `*.old` vengono cancellati all'avvio successivo.

Si installa solo una versione **più recente** di quella in uso, mai una
precedente. I download passano dal `curl.exe` di Windows (System32), solo in
HTTPS, e non servono diritti di amministratore. Dal terminale:
`wshare-cli update` (installa) o `wshare-cli update --check` (controlla e
basta).

Con l'**accesso non presidiato** attivo, il servizio usa una copia di wshare in
`Program Files` che l'aggiornamento non tocca: le Impostazioni lo segnalano e
basta premere «Aggiorna…» in «Accesso non presidiato» per copiarvi la nuova
versione.

## Compilare dai sorgenti

Servono Rust (la versione fissata in `rust-toolchain.toml`, che rustup
installa da solo: la stessa usata dalla CI e per le release) e il Windows SDK
(per `rc.exe`, che incorpora icona, manifest e informazioni di versione):

```
cargo build --release          # target\release\wshare.exe e wshare-cli.exe
cargo test --workspace
```

| Crate | Ruolo |
| --- | --- |
| `app` | l'eseguibile `wshare.exe`: finestra (egui), host, visualizzatore, servizio, aggiornamenti |
| `cli-shim` | `wshare-cli.exe`, il fronte da console di `wshare.exe` |
| `capture`, `codec`, `gpu`, `render`, `pipeline` | cattura, codifica/decodifica hardware e presentazione, tutto su GPU |
| `audio` | cattura (loopback) e riproduzione WASAPI |
| `net`, `protocol` | trasporto QUIC e formati dei messaggi |
| `discovery` | annuncio e ricerca dei PC nella LAN (mDNS) |
| `input`, `input_desktop` | iniezione di mouse e tastiera, desktop di input |
| `release` | manifest firmato degli aggiornamenti; il binario `wshare-release` lo genera e firma nella CI |

## Rilasci e aggiornamenti

Le release le crea GitHub Actions (`.github/workflows/release.yml`) quando si
pubblica un tag `v*`:

```
# 1. alza la versione in [workspace.package] di Cargo.toml, es. 0.2.0
cargo build                     # aggiorna Cargo.lock
git commit -am "Release v0.2.0"
git tag v0.2.0
git push origin main v0.2.0
```

La CI controlla che il tag corrisponda alla versione di `Cargo.toml`, compila
in release, prepara lo zip, genera `latest.json` con `wshare-release
manifest`, lo firma con `wshare-release sign` (che verifica anche che la firma
corrisponda alla chiave pubblica nel repository), calcola i `.sha256` e crea la
Release con le note generate dai commit. Asset di ogni release:

| Asset | A cosa serve |
| --- | --- |
| `wshare.exe`, `wshare-cli.exe` | scaricati dall'aggiornamento automatico |
| `wshare-<versione>-windows-x64.zip` | download manuale |
| `latest.json`, `latest.json.sig` | manifest firmato letto dall'app da `releases/latest/download/` |
| `*.sha256` | checksum per chi scarica a mano |

Gli altri workflow: `ci.yml` (fmt, clippy e test su ogni push a `main` e su ogni
pull request) e `cache-rust-main.yml` (scalda su `main` la cache Rust usata
dalle build dei tag).

### Chiave di firma

- **Pubblica**: `crates/release/update-key.pub` (hex, 32 byte), compilata in
  wshare. È pubblica, va committata.
- **Privata**: nel secret GitHub **`WSHARE_UPDATE_KEY`** (Settings → Secrets
  and variables → Actions), il documento PKCS#8 in hex. Una copia locale sta
  fuori dal repository (`%USERPROFILE%\.wshare\update.key`): **non va mai
  committata**, e ne va tenuto un backup sicuro.
- **Se la chiave privata va persa** le installazioni esistenti rifiuteranno
  ogni nuova release: servirà una nuova coppia (`wshare-release keygen --out
  FILE`, che stampa la chiave pubblica da mettere in `update-key.pub`) e
  un'installazione manuale una tantum su ogni PC.

## Accesso non presidiato

Come AnyDesk o RustDesk: usare un PC della LAN anche senza nessuno davanti,
compresi la **schermata di blocco**, l'**accesso a Windows**, le **richieste
UAC** e **Ctrl+Alt+Canc**. È facoltativo e spento per default: senza
attivarlo wshare funziona come sempre e niente richiede diritti di
amministratore.

### Attivarlo e disattivarlo

- **Dalla finestra**: Impostazioni → «Accesso non presidiato» → «Attiva…»
  (Windows chiede la conferma di un amministratore). «Aggiorna…» copia di
  nuovo i dispositivi autorizzati, cambia la password e ricopia le
  impostazioni usate dal servizio; «Disattiva…» toglie tutto.
- **Dal terminale**, come amministratore:

  ```
  wshare service install [--port P] [--monitor N] [--fps F] [--mbps B] [--codec h264|hevc]
                         [--no-clipboard] [--no-audio] [--password] [--from DIR]
  wshare service uninstall
  wshare service status
  ```

  `--password` chiede (due volte, senza mostrarla) una password che anche i
  dispositivi autorizzati dovranno inserire. `--from` indica la cartella dati
  da cui copiare identità e dispositivi autorizzati (di default quella
  dell'utente che esegue il comando; la finestra passa la propria). Le
  opzioni di condivisione funzionano come in `wshare host`.

Si connettono **solo i dispositivi già autorizzati**: prima di attivarlo,
connetti ogni PC che dovrà usarlo una volta con il PIN. Dopo averne
autorizzati altri, usa «Aggiorna…» (o ripeti `install`).

### Impostazioni rispettate

Il servizio non legge le Impostazioni della finestra: all'attivazione e a
ogni «Aggiorna…» la finestra gli passa (come argomenti del comando elevato,
la password solo come hash) quelle che hanno senso senza nessuno davanti, e
`install` le salva in `service.toml`:

| Impostazione | Con l'accesso non presidiato |
| --- | --- |
| Monitor, fotogrammi al secondo, bitrate massimo, codec, porta | applicate dopo «Aggiorna…» |
| Condividi appunti (testo e file copiati) | applicata dopo «Aggiorna…»: spenta, gli appunti restano privati |
| Condividi audio | applicata dopo «Aggiorna…» |
| Consenti controllo remoto | non si applica: il controllo è sempre consentito |
| Accesso (solo PIN, conferma…) | non si applica: solo dispositivi autorizzati, più la password se impostata |
| Ricevi file | non si applica: i file non vengono mai ricevuti (finirebbero come SYSTEM nel profilo di nessuno) |

Con il servizio installato, nel pannello le opzioni che non si applicano sono
disattivate con una nota, e se si cambia un'impostazione applicata la sezione
«Accesso non presidiato» ricorda di premere «Aggiorna…». Un `service.toml`
scritto da una versione precedente, senza queste impostazioni, si carica con
quello che il servizio faceva prima: monitor principale, 60 fps, 20 Mbit/s,
H.264, appunti e audio condivisi (basta «Aggiorna…» per allinearlo).

Nel visualizzatore il pulsante **«Ctrl+Alt+Canc»** della barra in alto invia
la sequenza all'host; se l'host chiede la password, la finestra la chiede
nella riga del computer.

### Architettura

Tre processi, tutti `wshare.exe`:

```
            ┌──────────────────────── PC host ─────────────────────────────┐
            │                                                              │
            │  Servizio "wshare" (sessione 0, LocalSystem)                 │
            │  wshare service run                                          │
            │   • segue la sessione alla console (notifiche WTS)           │
            │   • avvia/riavvia l'agente; job object: muore con lui        │
            │   • genera Ctrl+Alt+Canc (SendSAS)                           │
            │        │ stdin: chiusura = "fermati"                         │
            │        │ stdout: richieste ("secure-attention")              │
            │        │ stderr: log del servizio                            │
            │        ▼                                                     │
            │  Agente (sessione alla console, token SYSTEM,                │
            │  desktop winsta0\default)                                    │
            │  wshare service agent                                        │
            │   • è l'host: porta UDP, annuncio in LAN, identità           │
            │   • thread di cattura e input seguono il desktop di input    │◄── QUIC ── visualizzatore
            │     (Default ↔ Winlogon): blocco, accesso, UAC               │
            │   • ammette solo dispositivi autorizzati (+ password)        │
            │   • barra "… controlla questo PC" durante la connessione     │
            │                                                              │
            │  Finestra di wshare (sessione dell'utente)                   │
            │   • non condivide più questo PC: lo fa il servizio           │
            │   • installa/rimuove il servizio con elevazione UAC          │
            │   • resta visualizzatore verso gli altri PC                  │
            └──────────────────────────────────────────────────────────────┘
```

- **Servizio** (`unattended::service_main`, `supervisor`, `agent_process`):
  un solo agente, nella sessione collegata a schermo e tastiera
  (`WTSGetActiveConsoleSessionId`). A ogni cambio di sessione (accesso,
  disconnessione, cambio utente) ferma l'agente della vecchia e ne avvia uno
  nella nuova; un agente che termina troppo presto viene riavviato dopo una
  pausa. L'agente parte con una copia del token del servizio spostata nella
  sessione (`SetTokenInformation(TokenSessionId)`), unico modo di poter
  aprire il desktop sicuro.
- **Agente** (`unattended::agent`): l'host di sempre (`host::HostHandle`)
  con le opzioni del servizio. I thread di cattura (DXGI) e di input
  (`SendInput`) si spostano sul desktop che riceve l'input
  (`OpenInputDesktop` + `SetThreadDesktop`, crate `input_desktop`) quando la
  duplicazione perde l'accesso o un'iniezione viene bloccata. I file inviati
  dal visualizzatore non vengono ricevuti (finirebbero come SYSTEM nel profilo
  di nessuno); appunti e audio seguono `service.toml` (vedi «Impostazioni
  rispettate»).
- **Ctrl+Alt+Canc**: i tasti iniettati non lo generano mai. Il visualizzatore
  invia `SecureAttention` (protocollo 12); l'agente lo passa al servizio, che
  chiama `SendSAS`. Per la durata della chiamata il servizio abilita la policy
  `SoftwareSASGeneration` per i servizi e poi la riporta com'era (o la
  cancella se non c'era).
- **Convivenza con la finestra**: servizio e finestra userebbero la stessa
  porta, quindi la finestra **delega** la condivisione: con il servizio
  installato non avvia il proprio host e lo dice al posto dell'interruttore.
  Prima di installare ferma la propria condivisione.

### File

| Cosa | Dove | Chi può scrivere |
| --- | --- | --- |
| Copia di `wshare.exe` eseguita dal servizio | `%ProgramFiles%\wshare\` | amministratori |
| Identità (`cert.der`, `key.der`), dispositivi autorizzati, `service.toml`, `service.log` | `%ProgramData%\wshare-service\` | solo SYSTEM e amministratori (anche in lettura) |
| Servizio | `HKLM\SYSTEM\CurrentControlSet\Services\wshare` | gestito da Windows |

`uninstall` ferma e cancella il servizio (Windows ne rimuove la chiave) e
cancella le due cartelle; se un file è in uso viene cancellato al riavvio.
Nessun'altra chiave di registro resta modificata.

### Note di sicurezza

- **Solo dispositivi autorizzati**, riconosciuti dal certificato provato
  nell'handshake TLS: in modalità non presidiata PIN e conferma con clic non
  contano. Un dispositivo sconosciuto riceve sempre lo stesso rifiuto, senza
  sapere se c'è una password, e la sua password non viene nemmeno verificata
  (`unattended::rules`, con test).
- **Password**: salvata solo come hash PBKDF2-HMAC-SHA256 (`ring`), 600 000
  iterazioni, salt casuale di 16 byte, confronto a tempo costante. La
  finestra calcola l'hash nel proprio processo e passa al comando elevato
  solo l'hash. I tentativi sbagliati sono contati per indirizzo come quelli
  del PIN (pausa dopo troppi errori); la verifica gira su un thread a parte e
  solo per dispositivi autorizzati non in pausa. Il visualizzatore la manda
  nel campo `pin` del `Hello`, dentro il canale TLS.
- **Eseguibile del servizio**: il servizio gira come SYSTEM, quindi esegue
  una copia in `Program Files` (non sostituibile da un utente standard), mai
  il `wshare.exe` scaricato dall'utente.
- **Dati del servizio**: la cartella in `ProgramData` viene sempre ricreata
  da zero con un'ACL protetta (SYSTEM + Administrators, nessuna ereditarietà);
  una cartella preesistente, che un utente qualsiasi potrebbe aver creato,
  non viene mai riutilizzata.
- **Agente**: non sopravvive al servizio (job object con
  `KILL_ON_JOB_CLOSE`) ed eredita solo i tre handle delle sue pipe.
- **Visibilità**: durante una connessione l'agente mostra in cima allo
  schermo «Accesso non presidiato: … controlla questo PC», con il pulsante
  per disconnettere. Connessioni, rifiuti ed errori finiscono in
  `service.log`.
- **Limiti noti**: la barra non è visibile sopra la schermata di blocco (è su
  un altro desktop); il firewall di Windows deve consentire `wshare.exe` in
  `Program Files\wshare` sulla porta UDP scelta (di default 7420);
  `--from` copia i file della cartella indicata: chi installa (un
  amministratore) deve fidarsi di quella cartella.

### Verifica manuale

Su una macchina reale, con due PC:

1. sul visualizzatore connettiti all'host una volta con il PIN;
2. sull'host, Impostazioni → «Accesso non presidiato» → «Attiva…» (oppure
   `wshare service install` da un terminale amministratore); `sc query
   wshare` deve dire `RUNNING`;
3. premi Win+L sull'host, connettiti dal visualizzatore: si vede la
   schermata di blocco, si sblocca digitando la password di Windows;
4. apri una richiesta UAC (es. un terminale "come amministratore"): si vede
   e si conferma dal visualizzatore;
5. il pulsante «Ctrl+Alt+Canc» apre la schermata di sicurezza;
6. un PC mai autorizzato viene rifiutato («accetta solo i dispositivi già
   autorizzati»);
7. «Disattiva…» (o `wshare service uninstall`): `sc query wshare` non trova
   il servizio e le cartelle `%ProgramFiles%\wshare` e
   `%ProgramData%\wshare-service` non esistono più.
