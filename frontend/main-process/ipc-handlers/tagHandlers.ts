import { IpcMainEvent } from 'electron';
import { check_hardware_capability } from '../utils/hardwareDetector';
import { generateTagsWithAI } from './aiHandlers';

// 标签管理器
class TagManager {
    private manualTags: { [filePath: string]: string[] } = {};
    private aiTags: { [filePath: string]: string[] } = {};
    private aiEnabled: boolean;

    constructor() {
        this.aiEnabled = check_hardware_capability();
    }

    // 更新标签
    updateTags(filePath: string, manualTags: string[], aiTags?: string[]) {
        this.manualTags[filePath] = manualTags;
        if (this.aiEnabled && aiTags) {
            this.aiTags[filePath] = aiTags;
        }
    }

    // 获取合并后的标签
    getCombinedTags(filePath: string): string[] {
        const manual = this.manualTags[filePath] || [];
        const ai = this.aiEnabled ? (this.aiTags[filePath] || []) : [];
        return Array.from(new Set([...manual, ...ai.filter(tag => !manual.includes(tag))]));
    }
}

const tagManager = new TagManager();

// 处理标签生成请求
export async function handleGenerateTags(event: IpcMainEvent, filePath: string) {
    let aiTags: string[] = [];
    if (tagManager.aiEnabled) {
        aiTags = await generateTagsWithAI(filePath);
    }
    event.reply('tags-generated', aiTags);
}

// 处理标签更新请求
export function handleUpdateTags(event: IpcMainEvent, filePath: string, manualTags: string[]) {
    tagManager.updateTags(filePath, manualTags);
    event.reply('tags-updated', tagManager.getCombinedTags(filePath));
}

// 处理标签查询请求
export function handleQueryTags(event: IpcMainEvent, filePath: string) {
    const combinedTags = tagManager.getCombinedTags(filePath);
    event.reply('tags-queried', combinedTags);
}