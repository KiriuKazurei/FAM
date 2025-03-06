import { app, BrowserWindow, ipcMain, dialog } from 'electron';
import path from 'path';
import fs from 'fs';
import { check_hardware_capability } from '../preload-scripts/bridge'; // 假设硬件检测函数在这里
import { initializeAI, generateTags } from './ipc-handlers/aiHandlers'; // 假设AI处理函数在这里
import { handleFileOpen, handleFileSave, handleFileDelete } from './ipc-handlers/fileHandlers'; // 假设文件处理函数在这里
import { handleTagAdd, handleTagRemove } from './ipc-handlers/tagHandlers'; // 假设标签处理函数在这里

let mainWindow: BrowserWindow | null = null;

function createWindow() {
    mainWindow = new BrowserWindow({
        width: 1280,
        height: 720,
        webPreferences: {
            preload: path.join(__dirname, '../preload-scripts/preload.ts'),
            nodeIntegration: true,
            contextIsolation: false,
        },
    });

    if (process.env.NODE_ENV === 'development') {
        mainWindow.loadURL('http://localhost:3000');
    } else {
        mainWindow.loadFile(path.join(__dirname, '../../renderer-process/index.html'));
    }

    mainWindow.on('closed', () => {
        mainWindow = null;
    });
}

app.whenReady().then(() => {
    createWindow();

    app.on('activate', () => {
        if (BrowserWindow.getAllWindows().length === 0) {
            createWindow();
        }
    });

    // 检查硬件能力
    check_hardware_capability().then((isCapable) => {
        if (isCapable) {
            initializeAI().then(() => {
                console.log('AI模块已初始化');
            }).catch((error) => {
                console.error('AI模块初始化失败:', error);
            });
        } else {
            console.log('当前硬件配置不足，禁用AI功能');
        }
    });
});

app.on('window-all-closed', () => {
    if (process.platform !== 'darwin') {
        app.quit();
    }
});

// 文件处理的IPC通信
ipcMain.handle('open-file', handleFileOpen);
ipcMain.handle('save-file', handleFileSave);
ipcMain.handle('delete-file', handleFileDelete);

// 标签处理的IPC通信
ipcMain.handle('add-tag', handleTagAdd);
ipcMain.handle('remove-tag', handleTagRemove);

// AI处理的IPC通信
ipcMain.handle('generate-tags', generateTags);