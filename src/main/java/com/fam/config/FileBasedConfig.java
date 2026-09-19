package com.fam.config;

/**
 * @author Kazurei
 * @Description
 * @date 2025/6/15 17:52
 * @Project FileAutoManage(FAM)
 **/
public class FileBasedConfig implements ConfigService{
    private final String configFilePath;

    public FileBasedConfig(String configFilePath) {
        this.configFilePath = configFilePath;
    }

    @Override
    public SystemConfig loadConfig(){
        return ConfigLoader.loadFromResource(configFilePath);
    }

    @Override
    public void saveConfig(SystemConfig config) {
        //TODO: 保存逻辑
        System.out.println("保存配置到文件: " + configFilePath);
    }
}
