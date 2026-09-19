package com.fam.config;

import java.io.InputStream;
import java.util.Map;
import java.util.Properties;

/**
 * @author Kazurei
 * @Description
 * @date 2025/6/15 17:45
 * @Project FileAutoManage(FAM)
 **/
public class ConfigLoader {
    public static SystemConfig loadFromResource(String resourcePath) {
        Properties props = new Properties();
        try (InputStream input = ConfigLoader.class.getClassLoader().getResourceAsStream(resourcePath)) {
            if (input == null) {
                throw new RuntimeException("无法找到配置文件: " + resourcePath);
            }
            props.load(input);
            return new SystemConfig((Map<String, Object>) props);   //TODO waitng fix report
        } catch (Exception e) {
            throw new RuntimeException("加载配置失败", e);
        }
    }
}
