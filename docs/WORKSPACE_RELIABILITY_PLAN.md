# Rencana perbaikan workspace Chasm

**Status:** P0–P2 selesai (lihat Progres); P3–P4 berikutnya. **Baseline:** `UI` di `4c288d3`. Rilis baseline dipakai untuk membandingkan hasil sebelum dan sesudah perbaikan; membangun ulang binary tidak mengimplementasikan item di bawah.

## Tujuan dan kriteria lulus

| Urutan | Tujuan | Bukti untuk menyatakan selesai |
|---|---|---|
| P0 | Klien tahu kontrak mana yang dipilih dan apakah operasi benar-benar dimulai. | Pada dua tab dengan ABI sama, kesiapan UI dan API sepakat. Setiap permintaan yang ditolak punya alasan dan tidak membuat job; setiap operasi yang diterima punya satu job yang hasilnya bisa dibaca meski tab berpindah. |
| P1 | Kontrak terverifikasi bisa dibuka dari alamat tanpa menyusun workspace manual. | Dari fixture kontrak biasa dan proxy, pengguna dapat melihat sumber, asal ABI, blok verifikasi dan alamat tujuan panggilan sebelum memilih Attach. ABI ambigu, tidak cocok, atau tidak tersedia tidak dianggap terverifikasi. |
| P2 | Build dan debugger memakai unit kompilasi yang sama, atau menjelaskan mengapa tidak bisa. | Fixture dengan dua kelompok pragma yang independen berhasil di Auto; konflik dalam satu rantai impor menunjuk file penyebab. Untuk tiap artifact, build-info melaporkan versi solc dan settings yang benar. Source line yang tersedia tidak dijanjikan memiliki locals. |
| P3 | Fork dapat diuji dan dipulihkan secara berulang tanpa menyamakan identitas chain lokal dengan upstream. | Read menangani 429 dengan batas yang teruji; UI/API menyebut chain asal, chain eksekusi dan blok. Pengguna bisa mem-pin blok dan mengembalikan state lokal ke checkpoint lewat UI, dengan batas reopen dan umur checkpoint terlihat. |
| P4 | Pengguna tahu apakah rangkaian langkah itu simulasi, transaksi terpisah, atau rollback lokal. | Fixture dua langkah yang gagal di tengah memperlihatkan langkah yang sudah dikonfirmasi dan status state sesudahnya. Tidak ada klaim atomik atau rollback berhasil tanpa verifikasi. |

## Pekerjaan, berurutan

### P0 — Target dan hasil operasi (kerjakan pertama)

- Tambahkan regresi untuk tiga keadaan yang berbeda: artifact belum terikat, instance terikat, dan instance dari network epoch lama. Catat `documentId`, alamat dan epoch pada saat klik. Kasus laporan persis “alamat terisi tetapi Send meminta Attach” **belum direproduksi**; jangan menganggap kasus tab kosong sebagai buktinya.
- Pakai satu pemeriksaan kesiapan untuk `contracts.view`, tombol dan preflight eksekutor. Kembalikan `blockReason` bila ada penghalang. Bedakan memilih artifact dari memilih deployment. Validasi kembali saat eksekusi untuk menangani pergantian konteks.
- Bedakan **menunggu persetujuan**, **ditolak sebelum mulai**, **diterima sebagai job**, dan **hasil terminal**. Kaitkan `requestId → jobId → documentId` sehingga polling mengacu ke operasi, bukan tab aktif atau entri history terbaru. Pertahankan klien lama melalui schema berversi atau penambahan field yang kompatibel. Hasil terminal membedakan nilai kembali simulasi dari receipt Send; outcome tidak pasti tidak diulang otomatis.
- Uji pada dua tab, double submit dengan `requestId` sama, pergantian tab saat job berjalan, kegagalan validasi, revert, dan receipt tertunda. Pastikan hasil lama tidak ditampilkan sebagai hasil baru. Titik awal: `src/desktop/{application,native_contract,native_control,control}.rs`.

### P1–P2 — Import alamat, build dan locals

- Mulai dari **import read-only Sourcify → pratinjau → Attach**, bukan dari penambahan tiga provider sekaligus. Bedakan ABI untuk encoding, source untuk listing, dan build yang cocok untuk debugging. Pisahkan cache per chain/alamat/identitas kode; jangan eksekusi script atau menimpa source pengguna saat import. Tambah adapter Blockscout/Etherscan setelah perilaku cache, sumber tidak terverifikasi dan kegagalan provider punya tes.
- Saat Attach, tampilkan proxy dan implementation sebagai **dua alamat** pada blok yang sama. Target panggilan tetap alamat yang dipilih; asal ABI ditampilkan terpisah. Uji proxy fixture sebelum dan sesudah upgrade serta kasus resolver tidak mengenali proxy. Tidak menebak ABI dari nama atau selector saja.
- Samakan resolver build-info dengan build workspace: hormati Auto/Foundry/Chasm, lock solc, settings, serta dependency graph. Pilih build-info **per artifact/unit**, bukan memaksa semua artifact ke satu versi. Uji dua root kompatibel secara terpisah, satu import yang tidak kompatibel, dan lock eksplisit tanpa fallback diam-diam.
- Bedakan pesan “tidak ada AST”, “build tidak cocok”, dan “locals tidak dapat direkonstruksi pada codegen ini”. Hanya gunakan AST yang dapat dikaitkan dengan artifact; jika tidak, source stepping tetap tersedia. Titik awal: `src/{compiler,build_config}.rs`, `src/desktop/{sourcify,foundry_import,trace_locals}.rs`.

### P3 — Reliabilitas RPC, identitas fork dan fitur yang sudah ada

- Untuk **read idempoten saja**, tangani 429 dan `Retry-After` dengan jumlah percobaan, batas waktu total, backoff dan pembatalan. Bedakan limit provider dari limit control API. Jangan ulangi broadcast atau mutasi bila hasilnya tidak diketahui. Gunakan mock HTTP untuk `429 → sukses`, 429 berulang, timeout, dan pembatalan; hitung jumlah request serta pastikan URL berkredensial tidak masuk log.
- Tampilkan `sourceChainId`, `executionChainId`, blok/hash dan mode (workspace atau replay). Tetap batasi signing workspace ke chain 31337. Uji kontrak fixture yang membaca `block.chainid`, bukan hanya label UI; jangan menganggap replay transaksi di tengah blok selalu identik.
- Jangan bangun ulang Timeline. Tautkan **Deal ETH**, **Time/Timestamp**, **Mine**, **Snapshot/Revert**, **Code** dan **fork block → Save and reopen** dari panduan dan katalog; jelaskan tier/persetujuan serta efeknya. Uji checkpoint pada node lokal, ubah state/waktu, revert, lalu baca kembali nilai. Bedakan penggantian code pada node dari override sekali simulasi. Titik awal: `src/anvil.rs`, `src/desktop/{chain_read,contract_service,native_chain,toolchain}.rs`.

### P4 — Batas Macro dan pekerjaan lanjutan

- Pertahankan Macro untuk urutan transaksi. Di hasil run tampilkan langkah, hash, receipt, dan state terakhir yang dikonfirmasi. Bila kebijakan rollback lokal ditambahkan, wajib verifikasi checkpoint/probe; jangan melabelinya transaksi atomik. Uji kegagalan langkah kedua dan kegagalan rollback. Dry-run yang melewati mutasi harus menyatakannya.
- **Backlog terpisah, bukan janji rilis ini:** pemberian saldo token berbasis fixture (bedakan dari ETH dan model token berbasis share), pemodelan aktor dan override sekali simulasi di sandbox terisolasi, pemindahan fork block tanpa reopen, dukungan script/bundle dengan semantik atomisitas yang eksplisit, serta compiler bahasa selain Solidity. Evaluasi per kebutuhan; tidak menambahkan RPC arbitrer atau memperluas kewenangan signer untuk mencapai target ini.

## Verifikasi dan distribusi

Untuk tiap fase: tes unit negatif dan positif, satu flow desktop pada fixture lokal, lalu perbarui `AI_CONTROL.md`/`DEBUGGER.md` dan changelog dalam commit yang sama. Jangan gunakan kontrak produksi, endpoint privat, transaksi bertanda tangan yang direkam, atau workspace riset pengguna sebagai fixture.

Untuk retest Windows, catat revision sumber **dan** SHA-256 executable. Dari terminal baru, `(Get-Command chasm -CommandType Application).Source` harus menunjuk ke `target\release\chasm.exe` checkout yang dibangun; `chasm --help` harus berjalan. Build debug tidak memperbarui release di PATH. Bangun ulang release setelah setiap fase yang siap diuji. Tetap di branch `UI`; push hanya atas permintaan, tanpa merge ke `main`.

## Progres

| Fase | Status |
|---|---|
| P0 | Selesai: `call_block_reason` dipakai tombol, `contracts.view` (`sendBlockReason`, `readBlockReason`, `targetBound`) dan eksekutor. `contracts.send/simulate` lewat API: `failed/not_ready` + alasan tanpa job, atau `job_started` + `jobId` + `documentId`. `jobs.list` memberi `documentId`, `summary`, `transactionHash` untuk job panggilan kontrak. Live flow `call_jobs` (tab tanpa deployment ditolak, Send pada deployment terikat dilacak lewat job walau tab berpindah). Kasus “alamat terisi tetapi diminta Attach” tetap belum direproduksi; kini alasan penolakan terlihat sebelum dan sesudah klik. |
| P1 | Selesai untuk Sourcify: `proxy.rs` (slot EIP-1967 implementation/beacon, slot OpenZeppelin lama, clone EIP-1167) dipanggil saat Attach dan import; `contract_import.rs` + `contracts.import {address}` / tombol **Open verified contract** di Explorer membuka tab terikat dengan ABI dan layout Sourcify; proxy memakai ABI implementation; `abiOrigin` dan `proxy` di `contracts.view`. Import dimuat ulang dari cache tanpa jaringan. Diuji di fork mainnet (USDC → FiatToken, `name()` terdekode). Blockscout/Etherscan belum: perlu kunci API dan pengujian kegagalan provider. |
| P2 | Selesai: `debug_build_info` memakai `build_config::resolve` + `new_for_storage_layout_with_solc` (lock solc ikut), gagal jelas bila config rusak (tidak lagi fallback diam-diam), menulis `chasmSettings`; error multi-unit menyebut versi solc. Tes `debugger_build_info_follows_the_workspace_solc_lock` (0.8.26 terkunci menang atas 0.8.28; dua pragma eksak menyebut keduanya) dan round-trip ke worker Python lulus. `Session::locals_unavailable` membedakan kode tanpa build, Sourcify tanpa AST, step tanpa source map, dan via-IR/optimized. Build Auto tetap bisa memakai beberapa versi solc untuk root yang independen; worker tetap menerima satu build-info. |
