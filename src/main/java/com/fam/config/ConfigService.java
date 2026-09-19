package com.fam.config;

/**
 * @author Kazurei
 * @Description
 * @date 2025/6/15 17:50
 * @Project FileAutoManage(FAM)
 **/
public interface ConfigService {
    SystemConfig loadConfig();
    void saveConfig(SystemConfig config);
}
