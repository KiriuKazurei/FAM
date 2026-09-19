package com.fam.config;

/**
 * @author Kazurei
 * @Description
 * @date 2025/6/15 17:56
 * @Project FileAutoManage(FAM)
 **/
public class ConfigManager {
    private static volatile ConfigManager instance;
    private final ConfigService configService;
    private SystemConfig systemConfig;

    private ConfigManager(String configFilePath) {
        this.configService = new FileBasedConfig(configFilePath);
        this.systemConfig = configService.loadConfig();
    }

    public static ConfigManager getInstance(String configFilePath) {
        if (instance == null) {
            synchronized (ConfigManager.class) {
                if (instance == null) {
                    instance = new ConfigManager(configFilePath);
                }
            }
        }
        return instance;
    }

    public SystemConfig getConfig() {
        return systemConfig;
    }

    public void reload() {
        this.systemConfig = configService.loadConfig();
    }

    public void save() {
        configService.saveConfig(systemConfig);
    }
}
