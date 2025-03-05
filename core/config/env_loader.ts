import * as dotenv from 'dotenv';
import * as fs from 'fs';
import * as path from 'path';
import { Config } from './settings';
import { validateConfig } from './validator';

// 加载环境变量
dotenv.config();

// 默认配置
const defaultConfig: Config = {
    ai_enabled: false,
    force_enable_ai: false,
    thumbnail_quality: 0.7,
    max_threads: 2,
    preview_size: 1024,
    cache_ttl: 300,
    enable_hardware_accel: false
};

// 从环境变量加载配置
function loadEnvConfig(): Config {
    const envConfig: Config = {
        ai_enabled: process.env.AI_ENABLED === 'true',
        force_enable_ai: process.env.FORCE_ENABLE_AI === 'true',
        thumbnail_quality: parseFloat(process.env.THUMBNAIL_QUALITY || '0.7'),
        max_threads: parseInt(process.env.MAX_THREADS || '2', 10),
        preview_size: parseInt(process.env.PREVIEW_SIZE || '1024', 10),
        cache_ttl: parseInt(process.env.CACHE_TTL || '300', 10),
        enable_hardware_accel: process.env.ENABLE_HARDWARE_ACCEL === 'true'
    };
    return envConfig;
}

// 合并默认配置和环境变量配置
function mergeConfig(defaultConfig: Config, envConfig: Config): Config {
    return {
        ...defaultConfig,
        ...envConfig
    };
}

// 加载并验证配置
export function loadConfig(): Config {
    const envConfig = loadEnvConfig();
    const config = mergeConfig(defaultConfig, envConfig);
    if (!validateConfig(config)) {
        throw new Error('Invalid configuration');
    }
    return config;
}

// 示例用法
// const config = loadConfig();
// console.log(config);