import { check_hardware_capability } from './hardwareDetector'; // 假设硬件检测函数在 hardwareDetector.ts 中
import { IpcMainEvent } from 'electron';
import * as fs from 'fs';
import AIManager from './AIManager'; // 假设 AIManager 在另一个文件中定义

class TagManager {
    private manualTags: { [key: string]: string[] } = {};
    private aiTags: { [key: string]: string[] } = {};
    private aiEnabled: boolean = false;

    constructor() {
        this.aiEnabled = check_hardware_capability();
    }

    updateTags(filepath: string, manualTags: string[], aiTags?: string[]) {
        // 人工标签始终覆盖AI标签
        this.manualTags[filepath] = manualTags;
        if (aiTags && this.aiEnabled) {
            this.aiTags[filepath] = aiTags;
        }
    }

    getCombinedTags(filepath: string): string[] {
        const manual = this.manualTags[filepath] || [];
        const ai = this.aiEnabled ? (this.aiTags[filepath] || []) : [];
        return Array.from(new Set([...manual, ...ai.filter(tag => !manual.includes(tag))]));
    }

    checkAiEnabled(): boolean {
        return this.aiEnabled;
    }
}

const tagManager = new TagManager();

async function handleFileUpload(event: IpcMainEvent, filePath: string) {
    const thumbnail = await generateThumbnail(filePath);
    event.reply('thumbnail-ready', thumbnail);

    if (tagManager.checkAiEnabled()) {
        const aiTags = await generateAITags(filePath);
        tagManager.updateTags(filePath, [], aiTags);
    }
}

async function generateAITags(filePath: string): Promise<string[]> {
    // 假设有一个 AI 模块可以生成标签
    const aiModule = await AIManager.getInstance();
    if (aiModule.isAvailable) {
        const imageBuffer = fs.readFileSync(filePath);
        return await aiModule.generateTags(imageBuffer);
    }
    return [];
}