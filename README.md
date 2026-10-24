# Sekeeper

<img src="data/icons/hicolor/scalable/apps/io.github.sekeeper.Sekeeper.svg" width="104" height="104" align="right" alt="Icona di Sekeeper">

**Password manager desktop per Linux con vault cifrato e sincronizzazione
AWS S3 opzionale.**

Sekeeper è una piccola applicazione desktop scritta in Rust con interfaccia GTK4.
È pensata per custodire le credenziali personali in un archivio dedicato,
protetto da **password principale e file chiave**, con un uso quotidiano semplice:
apri il vault, cerca un account, consulta o aggiorna i suoi dati e blocca
l'applicazione quando hai finito.

Il vault rimane sul tuo computer e funziona anche senza connessione. Non serve
un account Sekeeper né un server dedicato. L'integrazione opzionale con un tuo
bucket AWS S3 aggiunge copie cifrate versionate, sincronizzazione tra PC e
recupero su una nuova installazione. La configurazione cloud resta sotto il tuo
controllo.

## Cosa puoi fare

- **Conservare le credenziali** con descrizione, username, password, indirizzo e note.
- **Cercare per descrizione**, anche parziale, senza distinzione tra maiuscole e
  minuscole, con le scorciatoie **Ctrl+F** ed **Esc**.
- **Vedere quando una voce è stata creata e modificata**, nell'elenco e nell'editor.
- **Proteggere l'accesso con due elementi indipendenti**: la password che ricordi
  e un file chiave casuale che custodisci separatamente.
- **Recuperare gli stati precedenti** tramite fino a tre backup locali cifrati.
- **Usare lo stesso vault su più PC Linux**, con sincronizzazione S3 opzionale,
  lavoro senza rete e scelta esplicita in caso di conflitti.
- **Bloccare il vault manualmente o automaticamente**, dopo cinque minuti di
  inattività; il tema chiaro/scuro segue la preferenza del desktop.

## Da dove iniziare

- [Requisiti e avvio](#requisiti-e-avvio)
- [Creare il primo vault](#creare-il-primo-vault)
- [Uso quotidiano](#uso-quotidiano)
- [Custodire il file chiave](#custodire-il-file-chiave)
- [Backup locali e recupero](#backup-locali-e-recupero)
- [Sincronizzazione AWS S3](#sincronizzazione-aws-s3-opzionale)
- [Sicurezza e formato dei dati](#sicurezza-e-formato-dei-dati)
- [Limiti attuali](#limiti-attuali)
- [Verifiche per lo sviluppo](#verifiche-per-lo-sviluppo)

## Requisiti e avvio

### Installazione su Arch Linux

La ricetta locale si trova in [`packaging/arch/PKGBUILD`](packaging/arch/PKGBUILD).
Da una copia completa del progetto, anche prima di creare un repository Git:

```sh
cd packaging/arch
makepkg -si
```

Esegui `makepkg` come utente normale. Il comando installa le dipendenze necessarie,
compila l'applicazione, esegue i test ordinari e propone l'installazione tramite
pacman. Servono gli strumenti di `base-devel` e una toolchain Rust; `rustup` è
utilizzabile se ha una toolchain predefinita compatibile. Al primo build è
necessaria la rete per scaricare le dipendenze Cargo.

Il pacchetto installa `sekeeper` in `/usr/bin`, la voce **Sekeeper** nel menu
applicazioni e l'icona nel tema `hicolor`, oltre a README e licenza. L'icona è
incorporata anche nell'eseguibile per l'avvio dalla cartella di sviluppo.
I dati personali in `~/.config/sekeeper/` e il file chiave non fanno parte del
pacchetto. La ricetta è per Arch Linux x86_64 e usa i sorgenti della copia locale;
non è una pubblicazione AUR e non richiede un URL GitHub.

Per creare soltanto il pacchetto usa `makepkg -s`, senza `-i`. Potrai installare
il file ottenuto con:

```sh
sudo pacman -U ./sekeeper-0.1.0-1-x86_64.pkg.tar.zst
```

Le dipendenze opzionali `curl` e `libsecret`, con un portachiavi Secret Service
attivo, servono per l'integrazione S3; non sono necessarie per l'uso locale.

### Compilazione manuale

Per compilare l'applicazione servono **Linux**, **Rust 1.89 o successivo** e le
librerie di sviluppo **GTK4 4.6 o successive**, con `pkg-config` e
`glib-compile-resources` per incorporare l'icona. Su Arch quest'ultimo è fornito
da `glib2`. Per l'uso locale non occorrono servizi cloud.

Per l'opzione AWS S3 servono anche `curl` 7.75 o successivo con supporto AWS
SigV4 e `secret-tool` (pacchetto `libsecret` su Arch Linux e `libsecret-tools`
su Debian/Ubuntu), con un portachiavi Secret Service attivo nella sessione.
Si usano i binari ELF installati in `/usr/bin/curl` e `/usr/bin/secret-tool`:
devono appartenere al proprietario dell'albero di sistema, senza link simbolici o
permessi di scrittura per gruppo/altri. Serve `/proc` montato, come nei normali
desktop Linux.

```sh
cargo build --locked --release
./target/release/sekeeper
```

Esegui i comandi dalla cartella del progetto. Dopo aver compilato una nuova
versione, chiudi l'istanza eventualmente aperta e riavvia l'eseguibile: una seconda
invocazione può limitarsi a portare in primo piano il processo già in esecuzione.

### Aspetto e integrazione con il desktop

Il tema chiaro/scuro segue la preferenza del desktop tramite il portale XDG,
anche quando cambia durante l'esecuzione. All'avvio vengono lette anche le
impostazioni GTK4 e, su KDE, la palette delle finestre come alternativa quando
il portale non è disponibile. Un eventuale `GTK_THEME` esplicito viene rispettato.
I selettori del file chiave sono finestre GTK interne e non richiedono il portale
di selezione file del desktop.

## Creare il primo vault

1. Avvia Sekeeper e scegli una **password principale** unica di almeno 15
   caratteri, preferibilmente una passphrase di 5–6 parole scelte casualmente.
2. Premi **Genera file chiave…** e salvalo fuori dalla cartella del vault, per
   esempio su una chiavetta USB cifrata già montata. Se ne possiedi già uno
   generato da Sekeeper, usa **Scegli file chiave…**.
3. Conserva una **copia identica del file chiave** in una seconda posizione
   separata, da usare per il recupero.
4. Conferma la password e premi **Crea vault**.
5. Premi **Aggiungi** per salvare la prima credenziale. Descrizione, username e
   password sono obbligatori; indirizzo e note sono facoltativi.

**Password principale e file chiave sono entrambi necessari.** Il file non è un
codice temporaneo: va conservato e riutilizzato, senza rigenerarlo a ogni avvio.
Se dimentichi la password o perdi tutte le copie del file chiave, non esiste una
procedura di recupero dei dati.

Se S3 è già configurato e vuoi iniziare con un vault vuoto, scegli **Crea un nuovo
vault vuoto** dalla schermata di recupero. Se il vault esiste già su un altro PC,
segui invece la procedura [per un PC nuovo](#recuperare-il-vault-su-un-pc-nuovo).

## Uso quotidiano

All'avvio inserisci la password principale, seleziona il file chiave e premi
**Sblocca**. Durante la sessione il percorso del file viene ricordato; dopo aver
chiuso e riaperto l'applicazione va selezionato nuovamente. Puoi scollegare la USB
dopo lo sblocco e continuare a lavorare fino al blocco successivo.

Usa **Aggiungi**, **Modifica** ed **Elimina** per gestire le voci. Puoi aprire
l'editor anche attivando una riga dell'elenco. Il pulsante **Blocca** chiude la
sessione e richiede di nuovo entrambi i fattori per accedere.

### Ricerca e date

La barra **Cerca per descrizione…** filtra le voci cercando anche parti della
descrizione, senza distinguere maiuscole e minuscole. **Ctrl+F** porta il cursore
alla ricerca; **Esc** la cancella. Il contatore indica quanti risultati sono
visibili rispetto al totale. Modifica ed eliminazione agiscono sulla voce
selezionata anche quando l'elenco è filtrato.

La data di creazione e l'ultima modifica compaiono nell'elenco e nella finestra
di modifica, nell'ora locale del PC. Sono conservate all'interno del vault cifrato
e nelle sue copie, comprese quelle su S3. La creazione resta invariata; l'ultima
modifica cambia solo quando vengono cambiati i dati della voce. Salvare senza
modifiche non crea una nuova versione.

Per eventuali date non registrate viene mostrato **Non disponibile**. Una
successiva modifica registra la data di modifica senza inventare quella di
creazione. Tutti i PC devono usare la versione con supporto al file chiave.

### Blocco automatico

Il vault si blocca dopo cinque minuti senza attività nell'applicazione, anche con
una finestra di modifica aperta. Il blocco chiude le finestre di modifica e scarta
le modifiche non salvate. Quando i servizi del desktop lo consentono, Sekeeper
si blocca anche al blocco o alla disattivazione della sessione e prima della
sospensione, tramite i segnali D-Bus di ScreenSaver e systemd-logind. Il timeout
rimane operativo anche senza questi servizi.

## Custodire il file chiave

Il file chiave è un secondo segreto necessario ad aprire il vault. Puoi tenerlo
su un supporto separato dal computer e dalle copie S3. La generazione tramite
Sekeeper non sovrascrive file esistenti.

Il file contiene un segreto casuale di 256 bit; entrambi i fattori sono necessari
alla derivazione della chiave di cifratura. Non basta rimuovere un controllo
nell'interfaccia per aprire il vault senza il file. Non esiste un ripiego alla
sola password. La funzione usa l'input segreto K previsto da
[Argon2, RFC 9106](https://www.rfc-editor.org/rfc/rfc9106.html#section-3.1).

Per ogni sblocco o recupero, seleziona il file e inserisci la password principale.
Il percorso viene ricordato solo durante la sessione dell'applicazione; a un nuovo
avvio va selezionato di nuovo. Una volta sbloccato puoi scollegare il supporto:
l'app mantiene soltanto la chiave derivata fino al blocco, permettendo salvataggi
e sincronizzazione senza rileggere il file chiave.

Conserva una **copia identica di recupero** su un altro supporto separato. Puoi
spostare o rinominare il file senza cambiare il vault; generare un nuovo file con
lo stesso nome produce invece una chiave diversa e non consente di aprirlo.
Se dimentichi la password o perdi tutte le copie del file chiave, i dati non sono
recuperabili. Non è previsto un cambio del file chiave su un vault esistente.

Il file non viene caricato su S3 né inserito nel vault, nei backup, nella
configurazione o nel portachiavi. Puoi custodirne una copia cifrata su un altro
cloud: prima dell'uso va decifrata in un file locale, con strumenti esterni.
Il file chiave non viene cifrato da Sekeeper: la cifratura del supporto USB o della
copia cloud va gestita separatamente. L'applicazione non monta supporti e non
decifra contenitori USB o file esterni.

### Supporti e permessi

Il file deve appartenere all'utente e avere permessi `0600` o più restrittivi;
sono rifiutati link simbolici e hard link. Anche i componenti del
percorso devono rispettare i controlli di proprietà e scrittura. La cartella scelta
deve già esistere, appartenere all'utente e non essere scrivibile da gruppo/altri;
i suoi permessi non vengono modificati. Usa un filesystem con permessi Unix, oppure
impostazioni di montaggio equivalenti. I buffer del segreto vengono azzerati dopo
la derivazione.

## Backup locali e recupero

I file si trovano in `~/.config/sekeeper/`:

| File | Contenuto |
| --- | --- |
| `vault` | Vault corrente cifrato |
| `vault.v3.bak.1` | Stato prima dell'ultimo salvataggio |
| `vault.v3.bak.2`, `vault.v3.bak.3` | Fino a due stati precedenti ulteriori |
| `vault.v3.before-restore` | File corrente conservato prima dell'ultimo ripristino |
| `vault.lock` | File usato per coordinare le scritture; non contiene password |
| `vault.s3.json` | Configurazione S3 locale, senza credenziali AWS |
| `vault.history-v3/` | Cronologia cifrata delle versioni predisposte per S3 |
| `vault.s3-cache/` | Copie cifrate scaricate, separate per destinazione S3 |

Le copie sono cifrate come il vault. Vengono aggiornate prima di ogni salvataggio;
alla sola creazione del vault non ci sono ancora versioni precedenti. Proteggono
da alcuni errori e cancellazioni, ma sono sullo stesso disco: conserva anche copie
su un supporto esterno, mantenendo una versione verificata prima di sostituirla.
Per copiare un insieme coerente di file, blocca o chiudi prima Sekeeper.

La cancellazione di una credenziale la rimuove dal vault corrente, ma non dalle
copie storiche che la contengono, locali o su S3.

### Ripristinare una copia locale

Per recuperare uno stato precedente:

1. Blocca il vault, se è aperto.
2. Premi **Ripristina una copia precedente…** nella schermata di sblocco.
3. Scegli una copia, inserisci la relativa password principale e seleziona il file chiave.
4. Conferma il ripristino. Le modifiche successive alla copia verranno annullate.

Il ripristino funziona anche se il file `vault` manca o è danneggiato. La copia
scelta viene autenticata e letta prima di sostituire il file corrente. Una password
o un file chiave errati, oppure una copia danneggiata, non modificano il vault
corrente. Se rimangono delle copie di backup, l'applicazione offre il recupero
anziché creare un vault vuoto.

Se viene segnalato un errore di conservazione dopo una scrittura, il file contiene
già le modifiche: Sekeeper mantiene lo stesso stato in memoria e non ripete
automaticamente l'operazione. La sopravvivenza delle modifiche a uno spegnimento
improvviso non è però confermata.

## Sincronizzazione AWS S3 (opzionale)

S3 permette di conservare una copia del vault fuori dal computer e condividere
lo stesso archivio tra più installazioni di Sekeeper. Ogni salvataggio
sincronizzato produce una copia cifrata identificata da una versione; il servizio
non riceve la password principale o il file chiave.

### Configurare il primo PC

Il pulsante **S3…** apre le impostazioni. La funzione è disattivata fino alla
configurazione. Inserisci bucket, regione (per esempio `eu-central-1`), prefisso
(per esempio `sekeeper/personale/`), nome del PC e credenziali IAM dedicate.
Il file chiave del vault non viene caricato su S3: trasferiscine una copia identica
sugli altri PC con un mezzo separato.
Il session token è facoltativo e serve se usi credenziali temporanee; quando queste
scadono, inserisci le nuove credenziali nelle impostazioni. Lasciando tutti i campi
delle credenziali vuoti vengono mantenute quelle già salvate.

Le credenziali AWS vengono conservate esclusivamente nel portachiavi locale tramite
`secret-tool`, separatamente dalla password principale. Non vengono scritte nel
JSON di configurazione, nel vault o negli oggetti S3. Se il portachiavi non è
disponibile, l'app segnala l'errore e non ripiega su file di credenziali in chiaro.
Usa lo stesso bucket e prefisso su tutti i PC, con credenziali dedicate a ciascun
dispositivo. Non usare credenziali amministrative o dell'account root.

### Bucket e permessi IAM

Il bucket deve essere privato, con tutte le opzioni Block Public Access attive,
ACL disabilitate e cifratura SSE-S3. È consigliato abilitare anche S3 Versioning.
Per iniziare, lascia disattivate le regole di scadenza automatica degli oggetti.

Questa policy IAM è sufficiente per l'integrazione; sostituisci `NOME_BUCKET`.
Assegnala all'identità dedicata, senza aggiungere altre policy S3 più ampie:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": "s3:ListBucket",
      "Resource": "arn:aws:s3:::NOME_BUCKET",
      "Condition": { "StringLike": { "s3:prefix": "sekeeper/*" } }
    },
    {
      "Effect": "Allow",
      "Action": "s3:GetObject",
      "Resource": "arn:aws:s3:::NOME_BUCKET/sekeeper/*"
    },
    {
      "Effect": "Allow",
      "Action": "s3:PutObject",
      "Resource": "arn:aws:s3:::NOME_BUCKET/sekeeper/*",
      "Condition": { "StringEquals": { "s3:if-none-match": "*" } }
    }
  ]
}
```

### Come avviene la sincronizzazione

Con il prefisso di esempio le copie vengono salvate come
`sekeeper/personale/v3/versions/<identificativo-casuale>.sek`. Ogni copia contiene un
vault interamente cifrato, con identificativo della versione, versioni precedenti,
nome del dispositivo e data protetti dalla stessa cifratura autenticata. La data
serve alla visualizzazione, non a decidere quale copia prevale.

La sincronizzazione avviene allo sblocco, dopo i salvataggi e ogni minuto mentre il
vault è aperto. Si può richiedere anche con **S3… → Sincronizza ora**. Il traffico
di rete e l'accesso al portachiavi avvengono su thread di lavoro. Una richiesta già
in corso può terminare dopo il blocco del vault, ma non può installare una copia
remota in una sessione bloccata o successivamente riaperta.

- Se la copia remota discende da quella locale, viene installata dopo la verifica
  crittografica e il backup locale. L'aggiornamento viene rimandato se c'è un editor
  aperto. Un'ulteriore modifica locale durante il trasferimento viene ricontrollata.
- Se ci sono versioni divergenti, vengono conservate e pubblicate entrambe. Apri
  **S3… → Versioni e conflitti…** e scegli esplicitamente quale contenuto usare.
  La scelta crea una nuova versione che riconosce entrambe le precedenti; non
  effettua una fusione automatica delle singole credenziali. Le copie precedenti
  restano recuperabili dallo stesso elenco.
- Senza rete, o con accesso AWS negato, il vault locale continua a funzionare. Le
  modifiche restano nella cronologia cifrata e vengono riproposte al prossimo
  tentativo. Un errore di predisposizione della cronologia successivo al salvataggio
  non annulla una modifica già salvata nel vault locale.
- Le copie remote vengono autenticate prima di pubblicare i dati locali verso una
  destinazione già popolata. Un vault diverso nello stesso prefisso, un file
  alterato o una cronologia incompleta interrompono la sincronizzazione.

### Recuperare il vault su un PC nuovo

Per un **PC nuovo**, configura S3 dalla schermata iniziale e premi **Recupera da
S3…**, usando la password principale e una copia identica del file chiave del
vault originale. Non creare un secondo vault indipendente nello stesso prefisso.
Se esistono più versioni divergenti, scegli quella da recuperare. Il recupero non
sovrascrive un vault o backup locali
già presenti: per un file locale danneggiato prova prima i backup locali; conserva
poi una copia dei file `vault` e `vault.v3.bak.*` prima di spostarli altrove e avviare
il recupero S3.

Se il bucket v3 è ancora vuoto, premi **Crea un nuovo vault vuoto** nella schermata
di recupero per iniziare da zero senza disattivare S3. Per gli altri PC recupera
poi il vault appena creato. La policy IAM sul prefisso `sekeeper/*` resta valida.

### Trasporto e limiti dello storico

Il trasporto usa HTTPS e la firma AWS SigV4 fornita da `curl`. Gli helper vengono
verificati e avviati dal descrittore del binario, evitando ricerche nel `PATH` o
una sostituzione del percorso tra controllo e avvio. Ricevono un ambiente minimo:
non ereditano override del loader, di GIO, dei certificati TLS o dei proxy.
Le chiavi vengono passate attraverso una pipe, non negli argomenti del processo.
Il corpo cifrato da caricare viene copiato in un `memfd` sigillato contro scritture
e ridimensionamenti; soltanto il processo di caricamento ne eredita il descrittore,
senza riaprire percorsi della cache sul disco. Ogni caricamento
usa `If-None-Match: *`: un oggetto esistente non viene sovrascritto, e una risposta
412 viene verificata scaricando la copia esistente. Non vengono eseguite operazioni
di cancellazione sul bucket. Le cache evitano di scaricare nuovamente lo storico
già presente sul PC. Ogni fase di trasferimento ha un limite di tempo; un invio
interrotto viene ripreso in un tentativo successivo senza duplicare le versioni.

Sono previsti limiti difensivi di 16 MiB per copia, 2048 versioni e 64 MiB di dati
cifrati per cronologia elaborata. Al raggiungimento del limite la sincronizzazione
si ferma, mantenendo utilizzabile il vault locale. Non è ancora disponibile una
compattazione automatica: eliminare antenati arbitrariamente interrompe la verifica
della cronologia. Le copie cifrate e la cache vengono mantenute anche disabilitando
S3; usa una versione aggiornata di Sekeeper su tutti i PC per preservare i metadati
di sincronizzazione. Restano utili backup indipendenti dal bucket.

## Sicurezza e formato dei dati

### Cifratura del vault

Il formato corrente è `SEKEEPER`, **versione 3**, con file chiave obbligatorio.
I precedenti formati v1 e v2 vengono rifiutati; non è prevista una migrazione.
Crea un nuovo vault dopo aver rimosso o archiviato quelli di prova. I backup, la
cronologia, la cache e lo spazio S3 del nuovo formato sono separati da quelli
precedenti. La configurazione S3 e le credenziali nel portachiavi possono essere
riutilizzate.

- Argon2id versione 0x13, **256 MiB**, **3 passaggi**, parallelismo **4**;
- file chiave con segreto casuale di 32 byte come input segreto K di Argon2id;
- chiave derivata di 32 byte, sale casuale di 16 byte;
- AES-256-GCM, nonce casuale di 12 byte e tag di 16 byte;
- JSON cifrato per tutti i campi delle credenziali;
- intestazione di 52 byte, autenticata integralmente come AAD di AES-GCM.

L'intestazione contiene magic, versione del formato, identificativi di KDF,
versione Argon2 e cifrario, costi Argon2 come tre interi little endian di 32 bit,
sale e nonce. Sono accettati soltanto i parametri del profilo v3: intestazioni con
costi ridotti, eccessivi o sconosciuti vengono rifiutate prima di eseguire il KDF.
Il profilo non dipende dai valori predefiniti delle librerie. La memoria di lavoro
Argon2 viene azzerata anche in caso di errore; un fallimento dell'allocazione
rilevabile viene segnalato senza ripiegare su parametri più deboli.

### Protezione dei file e del processo

Il vault, le copie e il file di coordinamento vengono scritti con permessi `0600`,
la directory con `0700`. Le scritture usano file temporanei e sostituzione atomica.
Le modifiche esterne vengono rilevate prima del salvataggio: in quel caso occorre
bloccare e riaprire il vault, invece di sovrascrivere silenziosamente altri dati.

Le directory vengono percorse un componente alla volta con `openat` e
`O_NOFOLLOW`, e letture, scritture, rinomine e pulizia dei temporanei usano
descrittori di directory mantenuti aperti. Sono rifiutati link simbolici, file non
regolari, hard link, proprietari inattesi e accessi a file leggibili/scrivibili da
gruppo o altri utenti. Un percorso di dati con link simbolici, anche in un
componente intermedio, non è supportato. La creazione di directory viene
sincronizzata anche nella directory superiore.

All'avvio, prima di inizializzare GTK o caricare segreti, il processo imposta
`PR_SET_DUMPABLE=0`, limiti soft e hard `RLIMIT_CORE=0`, `PR_SET_NO_NEW_PRIVS=1`
e `umask 077`. Se non può attivare queste protezioni, termina senza aprire il vault.
La protezione non modifica le impostazioni globali del computer. Gli helper
ereditano il limite sui core dump e il divieto di acquisire nuovi privilegi.

I buffer gestiti da Rust per password principale, chiave, credenziali, JSON in
chiaro e memoria di lavoro Argon2 vengono azzerati alla distruzione. Sono abilitate
anche le opzioni di azzeramento delle librerie crittografiche. Questo riduce le
copie residue, senza garantire l'assenza di ogni copia temporanea creata da GTK,
dal compilatore o dal sistema operativo. La cifratura protegge il file a riposo;
non protegge da un malware che osserva una sessione già sbloccata.

### Formato del file chiave

Il formato del file è `SEKEY001` (8 byte), segreto (32 byte), checksum SHA-256 dei
primi 40 byte (32 byte): 72 byte in totale. Il checksum rileva corruzioni accidentali;
non cifra il file.

## Limiti attuali

Sekeeper mantiene un ambito essenziale:

- È un'applicazione desktop per **Linux**; non include app per telefono,
  estensioni browser o compilazione automatica dei moduli.
- Non include un generatore di password per gli account o lo sblocco con codici
  Authenticator/TOTP. Il secondo fattore del vault è il file chiave.
- Non è previsto il cambio della password principale o del file chiave su un
  vault esistente dall'interfaccia.
- La sincronizzazione collega copie dello **stesso vault**. I conflitti richiedono
  la scelta di una versione completa, senza fusione delle singole voci.
- Non è disponibile una pulizia automatica della cronologia S3. Le versioni
  conservate occupano spazio e restano soggette ai limiti descritti sopra.
- I formati di prova v1 e v2 non vengono aperti dal formato corrente v3;
  non è prevista una migrazione.

La protezione del vault richiede una buona password principale e la custodia
separata del file chiave. I backup cifrati aiutano a recuperare i dati, ma non
sostituiscono la copia di recupero del file chiave.

## Verifiche per lo sviluppo

Il repository include sorgenti, `Cargo.lock`, risorse grafiche, ricetta Arch e
fixture pubbliche. Il `.gitignore` esclude directory di compilazione, pacchetti
generati, configurazioni locali, vault e file chiave personali. L'eccezione
`tests/fixtures/public-test.key` serve esclusivamente ai test e va mantenuta.
`Cargo.lock` va conservato per usare le versioni delle dipendenze già verificate.

```sh
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

Il test grafico richiede un display ed è escluso dai test ordinari. Con Xvfb:

```sh
xvfb-run -a cargo test --locked ui::tests -- --ignored
```

Verifica l'apertura dei selettori di generazione e scelta del file chiave,
creazione e sblocco con file chiave obbligatorio, salvataggi con il supporto
scollegato, ricerca e operazioni sulle voci filtrate, pulizia dell'editor al blocco
per inattività e ripristino dall'interfaccia. Gli altri test verificano generazione,
integrità e permessi del file chiave, rifiuto del secondo fattore errato, ricerca,
date, cifratura, alterazioni, rifiuto del vecchio formato, permessi, backup,
concorrenza ed errori di scrittura. Sono verificati anche dump e privilegi in processi isolati, gli
eseguibili, i file sigillati e gli accessi ostili al filesystem.
La fixture `tests/fixtures/vault-v3` contiene solo dati fittizi pubblici e usa la
password `public fixture master password` e il file `tests/fixtures/public-test.key`
(segreto pubblico di test, 32 byte `0x42`). Non usare mai questi valori per un vault
reale. Le fixture v1 e v2 verificano il rifiuto dei vecchi formati.

### Test di integrazione S3 reale

I test ordinari usano un servizio in memoria e un processo di trasporto fittizio;
non leggono credenziali AWS reali e non contattano bucket. Il test reale è escluso
per impostazione predefinita. Per eseguirlo configura esplicitamente queste
variabili nell'ambiente del processo, senza inserirle nel repository:

- `SEKEEPER_S3_TEST_BUCKET`
- `SEKEEPER_S3_TEST_REGION`
- `SEKEEPER_S3_TEST_ACCESS_KEY_ID`
- `SEKEEPER_S3_TEST_SECRET_ACCESS_KEY`
- `SEKEEPER_S3_TEST_SESSION_TOKEN`, solo se necessario

```sh
cargo test --locked s3::tests::aws_s3_live_roundtrip_and_conditional_upload -- --ignored
```

Il test carica solo un vault fittizio con password e file chiave di test pubblici,
sotto un nuovo prefisso `sekeeper/integration-test-…/`, e verifica lettura, elenco e protezione dalle
sovrascritture. Lascia l'oggetto nel bucket, perché l'identità non ha permessi di
cancellazione. È un'operazione reale soggetta alle tariffe AWS.

## Licenza

Codice e icona sono distribuiti con [licenza MIT](LICENSE).
