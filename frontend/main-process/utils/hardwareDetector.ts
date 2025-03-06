    import os from 'os';
    import { cpus } from 'os';
    
    // 检测 AVX2 指令集的函数（示例实现）
    function detectAVX2Support(): boolean {
        try {
            // 这里可以使用更复杂的逻辑来检测 AVX2 支持
            // 例如通过读取 CPU 特性或使用第三方库
            return true;
        } catch (error) {
            console.error('Failed to detect AVX2 support:', error);
            return false;
        }
    }
    
    export function check_hardware_capability(): boolean {
        // 内存检测
        const totalMem: number = os.totalmem() / (1024 * 1024); // MB
    
        // CPU线程数检测
        const threads: number = cpus().length;
    
        // 指令集检测
        const isAVX2Supported: boolean = detectAVX2Support();
    
        console.log(`Total Memory: ${totalMem} MB, Threads: ${threads}, AVX2 Supported: ${isAVX2Supported}`);
    
        return (totalMem >= 8192) && (threads >= 4) && isAVX2Supported;
    }