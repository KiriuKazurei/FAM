// validator.ts
import * as fs from 'fs';
import * as path from 'path';
import { ConfigurationError } from './error_handler';

// 配置验证规则（JSON Schema格式）
const configSchema = {
    type: 'object',
    properties: {
        ai_enabled: { type: 'boolean' },
        force_enable_ai: { type: 'boolean', default: false },
        memory_limit_mb: { type: 'number', minimum: 100, maximum: 8192 },
        thread_pool_size: { type: 'number', minimum: 1 },
        cache_policy: { 
            type: 'string', 
            enum: ['LRU', 'HYBRID', 'DISK_ONLY']
        },
        log_level: {
            type: 'string',
            enum: ['DEBUG', 'INFO', 'WARN', 'ERROR']
        },
        hardware_requirements: {
            type: 'object',
            properties: {
                min_ram_gb: { type: 'number', minimum: 1 },
                min_threads: { type: 'number', minimum: 1 },
                avx2_required: { type: 'boolean' }
            },
            required: ['min_ram_gb', 'min_threads']
        }
    },
    required: [
        'ai_enabled',
        'memory_limit_mb',
        'thread_pool_size',
        'cache_policy',
        'log_level'
    ]
};

/**
 * 配置验证器核心类
 */
export class ConfigValidator {
    private configPath: string;
    private hardwareDetector: any; // 与C++硬件检测模块的绑定接口

    constructor(configPath: string, hardwareDetector: any) {
        this.configPath = configPath;
        this.hardwareDetector = hardwareDetector;
    }

    /**
     * 执行完整配置验证流程
     * @returns 验证结果对象
     */
    public validate(): ValidationResult {
        const rawConfig = this.loadConfig();
        const schemaValidation = this.validateSchema(rawConfig);
        const hardwareValidation = this.validateHardware(rawConfig);
        const crossChecks = this.validateCrossConditions(rawConfig);

        return {
            valid: schemaValidation.valid && hardwareValidation.valid && crossChecks.valid,
            errors: [
                ...schemaValidation.errors,
                ...hardwareValidation.errors,
                ...crossChecks.errors
            ]
        };
    }

    // --- 私有辅助方法 ---

    private loadConfig(): any {
        try {
            const configJson = fs.readFileSync(this.configPath, 'utf-8');
            return JSON.parse(configJson);
        } catch (err) {
            throw new ConfigurationError(`配置文件加载失败: ${err.message}`);
        }
    }

    private validateSchema(config: any): ValidationStepResult {
        const errors: string[] = [];
        const Ajv = require('ajv');
        const ajv = new Ajv();
        const validate = ajv.compile(configSchema);

        if (!validate(config)) {
            validate.errors?.forEach((err: any) => {
                errors.push(`配置验证失败: ${err.instancePath} ${err.message}`);
            });
        }

        return {
            valid: errors.length === 0,
            errors: errors
        };
    }

    private validateHardware(config: any): ValidationStepResult {
        const errors: string[] = [];
        const hwRequirements = config.hardware_requirements || {};

        const detected = this.hardwareDetector.getHardwareInfo();
        if (detected.memoryMB < hwRequirements.min_ram_gb * 1024) {
            errors.push('内存不足: 需要至少' + hwRequirements.min_ram_gb + 'GB');
        }

        if (detected.logicalCores < hwRequirements.min_threads) {
            errors.push('CPU线程不足: 需要至少' + hwRequirements.min_threads + '线程');
        }

        if (hwRequirements.avx2_required && !detected.hasAVX2) {
            errors.push('未检测到AVX2指令集支持');
        }

        return {
            valid: errors.length === 0,
            errors: errors
        };
    }

    private validateCrossConditions(config: any): ValidationStepResult {
        const errors: string[] = [];

        // AI强制启用时的特殊检查
        if (config.force_enable_ai && !config.ai_enabled) {
            errors.push('force_enable_ai设置为true时需同时启用ai_enabled');
        }

        // 缓存策略与内存限制的关联检查
        if (config.cache_policy === 'LRU' && config.memory_limit_mb < 512) {
            errors.push('LRU缓存策略需要至少512MB内存限制');
        }

        return {
            valid: errors.length === 0,
            errors: errors
        };
    }
}

// 验证结果类型定义
export interface ValidationResult {
    valid: boolean;
    errors: string[];
}

export interface ValidationStepResult {
    valid: boolean;
    errors: string[];
}