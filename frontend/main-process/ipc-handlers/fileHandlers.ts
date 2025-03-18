// fileHandlers.ts
import { ipcMain } from 'electron';
import * as fs from 'fs'; // 修复：使用 * as fs 导入
import * as path from 'path'; // 修改为命名导入

/**
 * 注册文件相关的 IPC 处理器
 */
export function registerFileHandlers() {
    // 获取文件元数据
    ipcMain.handle('file:get-metadata', async (_event, filePath: string) => {
        try {
            const stats = await fs.promises.stat(filePath);
            return {
                size: stats.size, // 文件大小（字节）
                createdAt: stats.birthtime, // 创建时间
                modifiedAt: stats.mtime, // 修改时间
                isDirectory: stats.isDirectory(), // 是否为目录
            };
        } catch (error) {
            console.error(`Error reading file metadata for ${filePath}:`, error);
            throw new Error('无法读取文件元数据');
        }
    });

    // 搜索文件
    ipcMain.handle('file:search', async (_event, searchOptions: { directory: string; keyword: string }) => {
        const { directory, keyword } = searchOptions;
        try {
            const results: string[] = [];
            const files = await fs.promises.readdir(directory, { withFileTypes: true });

            for (const file of files) {
                const fullPath = path.join(directory, file.name);
                if (file.isDirectory()) {
                    // 如果是目录，递归搜索
                    results.push(...(await searchFiles(fullPath, keyword)));
                } else if (file.name.includes(keyword)) {
                    results.push(fullPath);
                }
            }
            return results;
        } catch (error) {
            console.error(`Error searching files in ${directory}:`, error);
            throw new Error('文件搜索失败');
        }
    });

    // 删除文件
    ipcMain.handle('file:delete', async (_event, filePath: string) => {
        try {
            await fs.promises.unlink(filePath);
            return { success: true, message: '文件删除成功' };
        } catch (error) {
            console.error(`Error deleting file ${filePath}:`, error);
            throw new Error('文件删除失败');
        }
    });
}

/**
 * 递归搜索文件
 * @param directory - 要搜索的目录路径
 * @param keyword - 搜索关键字
 * @returns 匹配的文件路径列表
 */
async function searchFiles(directory: string, keyword: string): Promise<string[]> {
    const results: string[] = [];
    const files = await fs.promises.readdir(directory, { withFileTypes: true });

    for (const file of files) {
        const fullPath = path.join(directory, file.name);
        if (file.isDirectory()) {
            results.push(...(await searchFiles(fullPath, keyword)));
        } else if (file.name.includes(keyword)) {
            results.push(fullPath);
        }
    }
    return results;
}
