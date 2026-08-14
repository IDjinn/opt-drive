// Processo main do Electron: spawn do daemon Rust (sidecar), descoberta da porta via
// stdout, e criação da janela com o renderer React.
const { app, BrowserWindow, shell } = require('electron');
const { spawn } = require('node:child_process');
const path = require('node:path');

const isDev = process.env.NODE_ENV === 'development';

/** @type {import('node:child_process').ChildProcess | null} */
let daemonProc = null;

/** Nome do binário do daemon conforme a plataforma. */
function daemonExeName() {
  return process.platform === 'win32' ? 'opt-drive-daemon.exe' : 'opt-drive-daemon';
}

/** Caminho do binário do daemon (dev = target/debug; prod = resources/bin). */
function daemonPath() {
  if (isDev) {
    return path.join(app.getAppPath(), '..', 'target', 'debug', daemonExeName());
  }
  return path.join(process.resourcesPath, 'bin', daemonExeName());
}

/** Spawna o daemon e resolve a base URL quando ele imprimir OPTDRIVE_LISTENING. */
function startDaemon() {
  return new Promise((resolve, reject) => {
    const exe = daemonPath();
    const args = [];
    if (process.env.OPT_DRIVE_CONFIG) args.push('--config', process.env.OPT_DRIVE_CONFIG);
    if (process.env.OPT_DRIVE_DB) args.push('--db', process.env.OPT_DRIVE_DB);
    if (process.env.OPT_DRIVE_PORT) args.push('--port', String(process.env.OPT_DRIVE_PORT));

    try {
      daemonProc = spawn(exe, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    } catch (e) {
      reject(new Error(`não foi possível iniciar o daemon (${exe}): ${e.message}`));
      return;
    }

    let resolved = false;
    const timer = setTimeout(() => {
      if (!resolved) reject(new Error('daemon demorou demais para responder'));
    }, 30000);

    daemonProc.stdout.on('data', (chunk) => {
      const text = chunk.toString();
      process.stdout.write(`[daemon] ${text}`);
      if (!resolved) {
        const m = text.match(/OPTDRIVE_LISTENING\s*(\{.*\})/);
        if (m) {
          try {
            const info = JSON.parse(m[1]);
            resolved = true;
            clearTimeout(timer);
            resolve(`http://127.0.0.1:${info.port}`);
          } catch {
            /* ignora linha malformada */
          }
        }
      }
    });

    daemonProc.stderr.on('data', (chunk) => {
      process.stderr.write(`[daemon:err] ${chunk.toString()}`);
    });

    daemonProc.on('error', (e) => {
      if (!resolved) {
        resolved = true;
        clearTimeout(timer);
        reject(new Error(`falha ao spawnar daemon: ${e.message}`));
      }
    });
  });
}

function stopDaemon() {
  if (daemonProc && !daemonProc.killed) {
    try {
      // No Windows, usa tree-kill via taskkill para encerrar filhos.
      if (process.platform === 'win32') {
        spawn('taskkill', ['/pid', String(daemonProc.pid), '/f', '/t']);
      } else {
        daemonProc.kill('SIGTERM');
      }
    } catch {
      /* ignore */
    }
  }
  daemonProc = null;
}

/** Cria a janela principal. */
function createWindow(apiBase) {
  process.env.OPT_DRIVE_API_BASE = apiBase;

  const win = new BrowserWindow({
    width: 1180,
    height: 780,
    minWidth: 900,
    minHeight: 600,
    backgroundColor: '#0e1116',
    title: 'Opt-Drive',
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
    },
  });

  // Links externos abrem no navegador do sistema.
  win.webContents.setWindowOpenHandler(({ url }) => {
    shell.openExternal(url);
    return { action: 'deny' };
  });

  if (isDev) {
    win.loadURL('http://localhost:5173');
    win.webContents.openDevTools({ mode: 'detach' });
  } else {
    win.loadFile(path.join(__dirname, '..', 'dist-renderer', 'index.html'));
  }
}

app.whenReady().then(async () => {
  try {
    const apiBase = await startDaemon();
    createWindow(apiBase);
  } catch (e) {
    // Sem daemon, ainda abre a janela mostrando erro.
    console.error(e);
    process.env.OPT_DRIVE_API_BASE = '';
    createWindow('');
  }

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow(process.env.OPT_DRIVE_API_BASE || '');
    }
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    stopDaemon();
    app.quit();
  }
});

app.on('before-quit', stopDaemon);
