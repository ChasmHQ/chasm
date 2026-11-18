# Rencana redesign UI native Chasm

Status: rencana, belum diimplementasikan. Ditulis 2026-11-15 setelah membandingkan langsung UI web (`chasm --web`) dan UI native (Slint) dengan Bank.sol.

Keputusan yang sudah diambil:
- UI tetap Slint (native), ditulis ulang strukturnya. Tidak ada WebView.
- Palet tetap tema native sekarang: grafit gelap + aksen emas.
- antislop dipakai selama perencanaan dan implementasi (mode during), ditutup Delivery Gate per fase.
- Sistem docking **tetap ada**. Layout bawaan baru menaruh panel hasil (response, hasil step macro) di zona kanan, dan pengguna tetap bisa memindahkannya.
- History hanya hidup selama sesi. Menyimpan ke disk belum diaktifkan; UI menampilkan catatan soal itu.
- UI web (`ui/`, `--web`) dihapus.
- Kerangka jendela tetap struktur Chasm sekarang: menu bar (logo, File, Workspace, Run, View, Panels, Tools, Help, teks status), toolbar kontekstual di bawahnya, rail ikon di kiri, status bar di bawah. Yang dirombak hanya isi area kerja.
- Setiap aktivitas mandiri: semua aksinya ada di toolbar dan panelnya sendiri, tidak perlu lewat menu Contracts. Antar-aktivitas tetap saling terhubung lewat aksi seperti "Kirim ke macro".
- Scrollbar memakai gaya Chasm sendiri, bukan bawaan sistem.

## 1. Masalah yang dibuktikan

| # | Masalah | Bukti |
|---|---|---|
| 1 | Kotak di samping tab pane | Grip tab memakai glyph `⋮` (U+22EE, `ui-slint/main.slint:621`). Font Segoe UI tidak punya glyph itu, jadi tampil sebagai kotak kosong. |
| 2 | Nama "Deploy · Bank" muncul tiga kali | Satu dokumen tampil sebagai tab dokumen, tab pane, dan judul halaman sekaligus. Di screenshot ada tiga strip: tab dokumen, tab pane (`Deploy · Bank`, `Investigation`), lalu heading. |
| 3 | Berpindah tab sangat sulit | Native punya sampai 5 tingkat tab: rail, tab dokumen, tab pane, Interact/Storage/Details, Simple/Raw/Trace. Deploy, panggil fungsi, lalu lihat trace butuh sekitar 9 klik (11 saat sempit). Web cukup sekitar 5 klik karena fungsi tampil langsung di sidebar. |
| 4 | Form tidak membantu | Semua argumen fungsi diketik di satu kolom JSON. Value hanya dalam wei. Web punya satu kolom per parameter, tombol tambah/hapus item array, dan pilihan unit ETH/gwei/wei. |
| 5 | Fitur web hilang di native | Snapshot dan revert per transaksi, cheatcodes (warp, roll, deal, prank), manajer OpenZeppelin global/proyek, fork dan RPC. Callback `warp/roll/snapshot/revert` sudah ada di Rust (`mod.rs`), tapi tidak ada tombol yang memanggilnya. |
| 6 | Macro sulit dibuat | Native hanya punya editor JSON mentah. Daftar step bersifat read-only (`macro.slint:324`). Web dulu punya step builder dengan dropdown instance dan fungsi. |
| 7 | Tidak ada jalan dari request ke macro | Tidak ada kode yang mengubah call atau Send menjadi step macro. |
| 8 | AI control terlalu sempit | Hanya tiga scope boolean (`read`, `ui`, `analyze`, `control.rs:20-26`). Deploy, send, run macro, compile, dan revert ditolak untuk semua klien. |
| 9 | Kontras garis tepi gagal | Token `edge` `#505050` di atas `#1e1e1e` hanya 2.07:1. Batas non-teks minimal 3:1. |
| 10 | Em dash di teks UI | `src/desktop/mod.rs:532` dan placeholder em dash (U+2014) di `session.rs` (beberapa baris). |

## 2. Arah desain

Design Read: workbench desktop untuk peneliti keamanan smart contract, dalam bahasa visual Chasm yang sudah ada (grafit gelap + emas), dial **ENERGY 1 / RHYTHM 2 / MOTION 1**.

| Keputusan | Alasan satu baris |
|---|---|
| Palet tetap: kanvas `#1e1e1e`, panel `#252525`, inset `#191919`, teks `#e0e0e0`, aksen emas `#d7ba7d` | Sudah lolos kontras (teks 12.6:1, emas 8.9:1) dan disukai pengguna. |
| Token `edge` dinaikkan ke `#767676` | Mencapai 3.37:1 di panel dan 3.67:1 di kanvas, lolos batas non-teks 3:1. |
| Emas hanya untuk satu aksi utama per layar dan penanda posisi (PC debugger, HEAD snapshot) | Aksen yang muncul di mana-mana berhenti berfungsi sebagai aksen. |
| Segoe UI untuk teks, Cascadia Mono untuk hex, alamat, calldata, dan trace | Font bawaan Windows. Huruf monospace membuat hex sejajar dan mudah dibandingkan. |
| Tanpa animasi selain hover/pressed | Alat kerja, bukan halaman pemasaran. Gerak tidak membantu membaca trace. |
| Status selalu teks + warna, tidak pernah warna saja | Penanda view/write, sukses/revert, dan confirmed harus terbaca oleh pengguna buta warna. |
| Motif identitas: label kecil berbentuk huruf mono (`VIEW`, `PAY`, `TX`, `REVERT`) di samping fungsi dan entri | Memberi rasa "alat chain" yang konsisten tanpa ornamen. |

## 3. Struktur baru (docking tetap, layout bawaan diganti)

Sistem docking empat zona, jendela melayang, dan menu posisi pane **tetap ada**. Yang diganti adalah layout bawaan dan aturan tampilnya, karena itulah yang menciptakan 3 sampai 5 tingkat tab dan nama ganda:
- Layout bawaan: navigasi di kiri, editor di tengah, **panel hasil di kanan** (Response untuk request, Hasil step untuk macro), drawer di bawah. Ini memakai zona Left/Center/Right/Bottom yang sudah ada di `layout.rs`.
- Satu pane per zona di layout bawaan, jadi strip tab pane tidak muncul. Strip itu hanya muncul bila pengguna sendiri menumpuk beberapa pane di satu zona.
- Nama tidak diulang: judul dokumen hanya ada di strip tab dokumen. Header pane menampilkan jenis pane (Request, Response, Hasil step), bukan judul dokumen lagi.
- Grip tab memakai ikon SVG, bukan glyph `⋮`.
- "Reset layout" mengembalikan layout bawaan ini.

```
+--+------------------+---------------------------------------------+
|R | Top bar: Jaringan | Akun | Compiler | Library | Recompile        |
|a +------------------+---------------------------------------------+
|i | Workspace         | Tab request (satu-satunya strip tab)        |
|l |  Contracts        +----------------------+----------------------+
|  |   Bank  [Deploy]  | Request              | Response             |
|  |  Deployed         |  kolom per parameter |  Decoded/Receipt/    |
|  |   Bank 0x5FbD…0aa3|  value + unit        |  Raw/Trace           |
|  |    VIEW balanceOf |  [Send] [Ke macro]   |                      |
|  |    PAY  deposit   |                      |                      |
|  |    TX   withdraw  |                      |                      |
|  |  Libraries        +----------------------+----------------------+
|  |                   | Drawer: Activity | History | Snapshots |     |
|  |                   |         Console  | Jobs                     |
+--+------------------+---------------------------------------------+
```

Aturan struktur:
- **Menu bar dan toolbar** tetap seperti sekarang. Toolbar berganti isinya sesuai aktivitas:
  - Contracts: Open project, Recompile, Deploy, Open storage, Layout.
  - Macros: New macro, Add step, Import JSON, Export JSON, Validate, Dry run, Run, Layout.
  - Debugger: Start, Continue, Step, Next, Finish, Breakpoint here, Cancel (toolbar debugger yang sudah ada).
  - Menu Run berisi aksi yang sama dengan toolbar aktif, dengan shortcut-nya.
- **Rail aktivitas** (urutan sekarang dipertahankan): Contracts, Explorer, Converter, Macros, Debugger, Wallet di bawah. Satu klik mengganti seluruh area kerja.
- **Sidebar Workspace**: kontrak dengan tombol Deploy di barisnya; instance ter-deploy dengan daftar fungsi langsung di bawahnya; library terpasang. Klik fungsi langsung membuka request editor.
- **Satu strip tab dokumen** (tab request, seperti browser). Response ada di pane kanan dan memakai toggle segmen, bukan tab bersarang. Di layout bawaan maksimal 2 tingkat pilihan di layar mana pun.
- **Deploy** membuka request editor dengan constructor, bukan dokumen terpisah. Setelah deploy sukses, instance baru terpilih dan fungsi pertama siap dipanggil.
- **Drawer bawah**: Activity (log), History (semua request), Snapshots (timeline + revert), Console, Jobs. Bisa ditutup dan diubah tingginya.
- **Debugger** memakai docking yang sama: kode di tengah, inspector di kanan, console di drawer.
- **Target klik**: deploy, panggil fungsi, lalu lihat trace dalam 5 klik atau kurang. Trace untuk Send di Anvil lokal dimuat otomatis.

## 4. Fitur

### 4.1 Request editor dan response
- Satu kolom per parameter, mengikuti tipe ABI:
  - `address`: input mono + pilihan cepat (akun Anvil, instance ter-deploy).
  - `uint/int`: angka + pilihan unit (wei/gwei/ether) bila nama parameter atau konteksnya nilai.
  - `bool`: toggle.
  - `bytes/bytesN/string`: input teks, bytes divalidasi hex.
  - Array: baris per item dengan Tambah/Hapus; array besar bisa ditempel sebagai JSON.
  - Tuple/struct: grup bersarang yang bisa dilipat.
- Mode Raw: calldata hex langsung, dengan decode otomatis bila selector cocok dengan ABI.
- Value dengan unit, gas limit Auto atau manual, pemilih akun pengirim.
- Validasi per kolom, pesan di bawah kolom yang salah. Tombol utama mati sampai semua kolom valid, dengan alasannya tertulis.
- Response: **Decoded** (nilai kembali per nama output), **Receipt** (status, gas, block, event ter-decode), **Raw** (JSON), **Trace** (pohon call). Shortcut: Ctrl+Enter kirim.

### 4.2 Snapshot, revert, dan cheatcodes
- Di Anvil milik Chasm, snapshot otomatis diambil sebelum setiap transaksi.
- Drawer **Snapshots** menampilkan timeline: fungsi, from, to, value, block, tx hash, status. Setiap entri punya tombol "Revert ke sebelum transaksi ini". Entri yang lebih baru ditandai akan hilang.
- Memakai logika `apply_revert` dan `network_epoch` yang sudah ada, diperluas per snapshot (bukan satu snapshot saja). Deployment yang dibuat setelah titik revert dihapus. Snapshot titik tujuan diambil ulang supaya bisa revert lagi.
- Panel **Chain** (di Tools atau drawer): warp (timestamp absolut atau `+detik`), roll (mine N block, konfirmasi di atas 1000), deal/setBalance dengan unit, prank/impersonate dan stop, setNonce, setCode, setStorage.
- Semua aksi chain lewat mutation gate yang sudah ada, jadi tidak bentrok dengan macro yang sedang berjalan.

### 4.3 Compiler dengan versi tertentu
- Chip compiler di top bar menampilkan versi aktif.
- Pilihan: **Auto dari pragma** (bawaan) atau **versi terkunci**. Daftar versi = yang sudah ada di `~/.svm` (saat ini 0.4.17 sampai 0.8.22 terpasang) + versi yang bisa diunduh.
- Mengunduh solc memakai fitur `svm-solc` dari foundry-compilers, sebagai job yang bisa dibatalkan, dengan progres.
- Pengaturan tambahan: optimizer on/off dan runs, EVM version, via-IR.
- Disimpan di `.chasm/chasm.toml` bagian `[compiler]` lewat `ChasmConfig::update` yang sudah aman (tidak menimpa key lain, tulis atomic).
- Pragma yang tidak cocok dengan versi terkunci dilaporkan per file sebelum compile.

### 4.4 Manajer library (global dan proyek)
- Daftar library yang dikenal beserta prefix remapping:
  - OpenZeppelin Contracts (`@openzeppelin/contracts/`)
  - OpenZeppelin Contracts Upgradeable (`@openzeppelin/contracts-upgradeable/`)
  - forge-std (`forge-std/`), solmate (`solmate/`), solady (`solady/`)
- Dua cakupan:
  - **Global**: `~/.chasm/lib/<nama>@<versi>`, dipakai ulang antarproyek, tanpa unduh ulang.
  - **Proyek**: `lib/<nama>` di dalam proyek.
- **Pasang otomatis**: saat compile gagal karena import seperti `@openzeppelin/contracts/token/ERC20/ERC20.sol` tidak ditemukan, Chasm mengenali prefixnya dan menawarkan "Pasang OpenZeppelin 5.x (global)" dalam satu klik. Versi yang ditawarkan menyesuaikan pragma (OZ 5 butuh ^0.8.20). Bila versi itu sudah ada secara global, cukup dihubungkan tanpa jaringan.
- Pemasangan memakai `git clone --depth 1 --branch v<versi>` sebagai job yang bisa dibatalkan. Script di dalam repo library tidak pernah dijalankan.
- Template kontrak baru: ERC20, ERC721, Ownable (OpenZeppelin). Membuat file di `contracts/` dan memasang library bila belum ada.

### 4.5 History dan "Kirim ke macro" (seperti Burp)
Pemetaan ke Burp: History = Proxy history, request editor = Repeater, macro = Macros.
- **History**: setiap call, simulate, send, dan deploy dicatat dengan data publik saja: waktu, jenis, kontrak, fungsi, argumen ter-decode, target, value, gas, alamat pengirim, status, tx hash, block. Tidak pernah menyimpan private key, password, atau payload bertanda tangan.
- History hanya hidup selama aplikasi terbuka. Menyimpan ke disk (`.chasm/history.jsonl`) **belum diaktifkan**. Drawer History menampilkan catatan tetap: "History hanya tersimpan selama aplikasi terbuka. Simpan ke disk belum tersedia." Batas 500 entri per sesi; entri terlama dibuang dengan pemberitahuan.
- Aksi per entri: Buka di editor, Kirim ulang, **Kirim ke macro**, Bandingkan dengan entri lain, Salin sebagai perintah `cast`.
- **Kirim ke macro** (juga tombol di request editor, shortcut Ctrl+M):
  1. Pilih macro tujuan (yang sudah ada atau baru) dan posisi sisip (akhir, atau setelah step terpilih).
  2. Request menjadi step `AbiCall` bila kontraknya dikenal, atau `RawCall`/`RawSend` dari target dan calldata.
  3. **Parameterisasi**: alamat target yang dibuat oleh step deploy sebelumnya ditawarkan menjadi `${step.<id>.contractAddress}`; pengirim yang sama dengan akun Anvil N menjadi `AnvilAccountN`. Setiap penggantian bisa ditolak satu per satu.
- Batasan saat ini: `AbiCall` mewajibkan artifact Foundry (`macro_adapter.rs`). Model macro diperluas ke versi skema 2 supaya bisa merujuk kontrak hasil compile workspace biasa (nama kontrak + digest ABI), dengan migrasi dari skema 1.

### 4.6 Macro builder
Macros adalah aktivitas mandiri. Membuat macro tidak perlu lewat menu Contracts:
- **New macro** di toolbar membuat macro kosong atau dari template.
- **Add step** (toolbar dan tombol `+` di header daftar step) membuka menu jenis step: Panggil fungsi kontrak, Deploy kontrak, Raw call/send, Baca storage, Assert, Snapshot, Revert ke snapshot, Gas sweep, Delay, dan "Dari History…". Step baru disisipkan setelah step terpilih.
- Untuk step Panggil fungsi dan Deploy, kontrak, instance, dan fungsi dipilih lewat dropdown di form, dari kontrak yang sudah di-compile dan instance yang sudah di-deploy.
- **Import JSON** dan **Export JSON** tetap tersedia, termasuk file macro lama (skema 1 dimigrasikan otomatis).
- Di view Macros, sidebar Workspace disembunyikan supaya tiga kolom mendapat ruang penuh.

Tiga kolom di layout bawaan, semuanya pane docking biasa sehingga bisa dipindah:
- **Kiri (navigasi)**: daftar step berurutan (ListView, sampai 128 step). Per baris: nomor, nama, jenis, toggle aktif, status run terakhir (OK, Gagal, Dilewati, belum dijalankan) beserta durasi. Aksi: naik/turun (juga Alt+Up/Alt+Down), duplikat, hapus, jalankan step ini saja.
- **Tengah (editor)**: form step sesuai jenisnya (Deploy, AbiCall, RawCall, RawSend, StorageRead, Snapshot, Revert, GasSweep, ProbeGasSweep, Assert, Delay). Dropdown untuk instance, fungsi, dan signer.
- **Kanan (Hasil step)**: hasil run terakhir untuk step yang dipilih:
  - Ringkasan: status, durasi, gas, block, tx hash.
  - Request yang sebenarnya dikirim, dengan semua `${...}` sudah diganti nilainya, supaya terlihat referensi mana yang menghasilkan nilai apa.
  - Output ter-decode (return value, event, storage yang dibaca), lalu Raw dan Trace sebagai toggle segmen.
  - Untuk Assert: nilai kiri, operator, nilai kanan, dan alasan gagal.
  - Untuk GasSweep: tabel percobaan (gas, hasil) dengan gas terpilih ditandai.
  - Bila step belum pernah dijalankan: "Belum dijalankan. Jalankan macro atau step ini untuk melihat hasilnya."
- Memilih step di kiri mengganti editor dan panel hasil sekaligus. Selama run, panel hasil mengikuti step yang sedang berjalan kecuali pengguna memilih step lain.
- **Pemilih referensi**: setiap kolom yang menerima nilai menampilkan daftar `${...}` yang valid dari step sebelumnya, jadi pengguna tidak perlu menghafal sintaks.
- Header editor punya toggle **Form / JSON**. JSON memakai fitur editor JSON yang sekarang (validasi, error per baris) dan tersinkron dua arah dengan form: perubahan valid di JSON langsung tampil di Form, perubahan tidak valid ditahan dengan pesan error tanpa menghapus isi Form.
- Validasi berjalan saat mengetik. Error menunjuk ke step dan kolomnya.
- Template awal: "Deploy lalu panggil", "Snapshot, aksi, assert, revert".

### 4.7 AI control tiga tingkat
Model scope boolean diganti tingkat per klien, dipilih saat pairing, bisa diubah dan dicabut kapan saja.

| Kemampuan | Limited | Partial | Full |
|---|---|---|---|
| Baca state publik, ABI, deployment, chain read, daftar macro, hasil run | Ya | Ya | Ya |
| Navigasi UI, pilih kontrak/fungsi, isi form | Tidak | Ya | Ya |
| Analisis (history, cases, simulate eth_call) | Sebagian (baca saja) | Ya | Ya |
| Deploy dan send di Anvil milik Chasm (31337) | Tidak | Persetujuan per aksi | Ya |
| Run macro, snapshot/revert, cheatcodes | Tidak | Persetujuan per aksi | Ya |
| Compile, ubah versi compiler, pasang library | Tidak | Persetujuan per aksi | Ya |
| Perintah debugger | Tidak | Tidak | Ya |
| Kirim ke jaringan selain Anvil milik Chasm | Tidak | Tidak | Hanya bila diaktifkan pengguna per sesi, dengan konfirmasi |

Selalu ditolak di semua tingkat:
- Membaca private key, password keystore, mnemonic, atau payload bertanda tangan.
- Ekspor atau hapus keystore.
- Mengubah pengaturan AI access itu sendiri.

Pengaman:
- Setiap mutasi oleh klien Full dicatat di **audit log** yang terlihat di Settings, berisi klien, aksi, parameter publik, dan hasil.
- Tombol "Cabut semua" selalu tersedia.
- Klien Full bisa dipasang mode "konfirmasi setiap mutasi" atau "percaya sampai aplikasi ditutup".

Implementasi:
- Tabel kemampuan per aksi di `application.rs`, ditegakkan di `Hub::execute_ui`.
- Katalog dan daftar tool MCP dibangkitkan dari tabel itu.
- Test sentinel rahasia yang sudah ada diperluas ke semua aksi baru.

## 5. Aturan agar UI tidak rusak oleh data panjang

| Kasus | Aturan |
|---|---|
| Alamat dan hash | Dipendekkan di tengah (`0x5FbD…0aa3`) dengan tombol salin. Nilai penuh ada di tooltip dan saat dipilih. |
| Nama fungsi atau kontrak panjang | Dipotong dengan "…" dan nama penuh di tooltip. Tidak pernah memotong glyph. |
| Output panjang (trace, raw JSON, output step) | Kotak dengan tinggi maksimum dan scroll sendiri, scroll horizontal untuk mono. Di atas 1 MiB tampil "Terlalu besar untuk ditampilkan, simpan ke file". |
| Daftar panjang (step macro, history, snapshot, deployment, log) | Selalu ListView tervirtualisasi, tidak pernah `for` loop tanpa batas tinggi. |
| Output step macro | Satu baris ringkasan per step. Klik untuk membuka detail di panel bawah yang bisa di-scroll, bukan melebarkan baris. |
| Banyak tab request | Strip tab bisa di-scroll, tab aktif selalu terlihat penuh, ada menu "semua tab". |
| Teks bantuan panjang | Membungkus kata di dalam kolomnya, tidak pernah menambah lebar container. |
| Scrollbar | Komponen scrollbar Chasm sendiri untuk semua area yang bisa di-scroll: lebar 8 px, track transparan, thumb abu `#4a4a4a` dengan sudut membulat, berubah emas saat hover atau diseret, tanpa tombol panah. Muncul saat kursor di atas area atau saat sedang di-scroll, dan tetap bisa diseret serta dipakai dengan keyboard. |
| Karakter khusus | Hanya glyph yang tersedia di Segoe UI dan Cascadia Mono. Ikon memakai SVG, bukan simbol Unicode. |

Pembuktian, selain review mata:
- Test capture yang sudah ada (`view_capture_tests.rs`) diperluas dengan fixture stres:
  - Macro 128 step, output trace 64 KiB.
  - Nama fungsi 60 karakter, fungsi 20 parameter, array 50 item.
  - 30 deployment, 15 tab request, 200 entri history.
- Matriks: 1180x720, 1360x820, 1600x960 pada zoom 100, 110, 125, 150.
- Review visual per fase dengan daftar cacat bernomor. Fase dianggap selesai hanya bila tidak ada cacat tumpang tindih, terpotong tanpa "…", atau kontrol yang tidak terjangkau.
- Test interaksi Slint untuk alur utama (deploy, panggil, trace; kirim ke macro; revert) lewat backend testing yang sudah dipakai `copy_tests.rs`.
- Uji di aplikasi yang benar-benar berjalan untuk alur utama tiap fase, seperti deploy Bank tadi.

## 6. Urutan pengerjaan

Setiap fase berdiri sendiri: di-commit terpisah, lulus `cargo test`, lulus review visual, dan dicoba di aplikasi yang berjalan.

| Fase | Isi | Selesai bila |
|---|---|---|
| 0. Perbaikan cepat | Ganti glyph `⋮` dengan grip SVG, hapus em dash, `edge` ke `#767676`, setelah deploy pindah ke instance baru, hapus UI web (`ui/`, `--web`, `include_dir!`) dan dependensi Node pada build, komponen scrollbar `ChasmScroll` dipakai di semua area scroll | Tidak ada kotak/tofu; deploy langsung siap dipanggil; `cargo build` jalan tanpa Node |
| 1. Shell baru | Layout bawaan baru di atas docking yang ada (navigasi kiri, editor tengah, hasil kanan, drawer bawah); header pane tanpa nama ganda; sidebar dengan fungsi per deployment; deploy lewat request editor | Deploy, panggil, trace dalam 5 klik atau kurang; maksimal 2 tingkat pilihan di layout bawaan; pane tetap bisa dipindah dan Reset layout mengembalikannya |
| 2. Request editor | Kolom per parameter, unit, Raw calldata, Decoded/Receipt/Raw/Trace, trace otomatis | Semua tipe ABI Bank dan fixture stres bisa diisi tanpa JSON |
| 3. Chain | Timeline snapshot + revert per transaksi, panel Chain (cheatcodes), chip jaringan | Revert ke titik mana pun menghapus deployment yang lebih baru dan tetap konsisten |
| 4. Compiler dan library | Versi terkunci/Auto, unduh solc, manajer library global/proyek, pasang otomatis dari error import, template ERC20 | Kontrak `import "@openzeppelin/.../ERC20.sol"` compile setelah satu klik |
| 5. History dan macro | History sesi (tanpa simpan ke disk, dengan catatan), Kirim ke macro dengan parameterisasi, macro builder tiga kolom dengan panel Hasil step di kanan, pemilih referensi, skema macro 2 | Request deposit menjadi step macro yang lolos validasi tanpa menyentuh JSON; setiap step menampilkan hasilnya di kanan |
| 6. AI control | Tingkat Limited/Partial/Full, audit log, tabel kemampuan, test rahasia | Setiap aksi di tabel teruji per tingkat; kunci tidak pernah keluar |
| 7. Penutup | Fixture stres lengkap, review visual akhir, dokumentasi | Delivery Gate antislop bersih di semua blok |

Fase 0 dan 1 paling besar dampaknya terhadap keluhan "berpindah tab susah", jadi dikerjakan lebih dulu.

## 7. Yang dihapus dan yang dipertahankan

Dihapus:
- UI web (`ui/`, flag `--web`, server Axum untuk halaman web, `include_dir!("ui/dist")`) di Fase 0, bersama dependensi Node pada build (milestone M12). Gateway debugger di `src/debugger` tetap, karena dipakai oleh debugger native.
- Dokumen terpisah untuk Deploy dan Wallet; keduanya pindah ke request editor dan Settings.
- Preset layout lama (Contracts/Debugging/Investigation) diganti satu layout bawaan per aktivitas. Layout tersimpan lama yang tidak cocok diabaikan dengan backup yang sudah ada.

Dipertahankan:
- Sistem docking empat zona, jendela melayang, menu posisi, maximize, dan Reset layout.
- Tab pane, tetapi hanya muncul bila pengguna menumpuk pane di satu zona.

## 8. Risiko dan pertanyaan terbuka

- Slint tidak punya rich text. Trace berwarna per jenis call perlu dibangun sebagai daftar baris berwarna (ListView), bukan satu blok teks.
- Seret untuk mengurutkan step tidak tersedia bawaan di Slint. Fase 5 memakai tombol naik/turun dan Alt+panah dulu.
- Scrollbar bawaan Slint (gaya Fluent) tidak bisa diganti gayanya per komponen. Scrollbar custom berarti membuat komponen sendiri `ChasmScroll` di atas `Flickable` (thumb, track, seret, roda mouse, keyboard) dan memakainya untuk ke-44 `ScrollView`/`ListView` yang ada sekarang. Daftar panjang yang butuh virtualisasi (step macro, history) tetap memakai model `ListView` dengan scrollbar custom yang dipasang di sampingnya. Dikerjakan di Fase 0 supaya semua fase berikutnya langsung memakainya.
- Tingkat Full memberi AI kemampuan mengirim transaksi. Bawaannya hanya ke Anvil milik Chasm; jaringan lain harus dinyalakan pengguna per sesi.
- Docking tetap ada, jadi kombinasi posisi pane yang dipilih pengguna bisa tetap sempit. Aturan di bagian 5 berlaku untuk setiap pane di setiap zona, dan fixture stres dijalankan juga dengan pane di zona lain.
- Menghapus UI web juga menghapus fitur web-only yang belum diport (cheatcodes, timeline snapshot, manajer library). Semuanya ada di Fase 3 dan 4, jadi ada jeda saat fitur itu belum tersedia di mana pun.
