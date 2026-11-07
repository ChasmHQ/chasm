# Chasm Web UI

Chasm dapat dijalankan melalui browser lokal tanpa mengubah cara kerja debugger atau
perintah gdb-style yang tersedia di terminal.

## Menjalankan UI web

Jalankan contoh Bank yang tersedia di repository dari root proyek:

```bash
uv run chasm run --web --contracts examples/bank/src examples/debug_bank.py
```

Chasm akan membuka browser secara otomatis. Jika browser tidak terbuka, akses alamat
lokal berikut:

```text
http://127.0.0.1:8765
```

Server hanya menerima koneksi dari komputer lokal. Tekan `Ctrl+C` pada terminal yang
menjalankan Chasm untuk menghentikan server dan melepas sesi debugger.

## Opsi akses

Gunakan port lain apabila port `8765` sedang digunakan:

```bash
uv run chasm run --web --web-port 9000 --contracts examples/bank/src examples/debug_bank.py
```

Gunakan `--no-browser` bila ingin membuka alamatnya sendiri:

```bash
uv run chasm run --web --no-browser --contracts examples/bank/src examples/debug_bank.py
```

Kemudian buka `http://127.0.0.1:8765` atau port yang dipilih.

## Menggunakan debugger

Tombol Continue, Step in, Step over, Breakpoint, dan Restart mengirim perintah yang sama
dengan frontend terminal. Kolom prompt juga menerima perintah Chasm, misalnya:

```text
p amount
step
continue
break Bank.sol:4
```

Panel source, disassembly, stack, call stack, locals, storage, dan command log memperbarui
data dari sesi debugger yang sedang berjalan.
