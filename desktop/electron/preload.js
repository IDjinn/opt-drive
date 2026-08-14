// Preload: expõe ao renderer a base URL do daemon (descoberta pelo main).
const { contextBridge } = require('electron');

contextBridge.exposeInMainWorld('optDrive', {
  // definido em main.js antes da criação da janela
  baseUrl: process.env.OPT_DRIVE_API_BASE || '',
});
