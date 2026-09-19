package com.fam.config;

import java.util.Map;

/**
 * @author Kazurei
 * @Description 存储系统配置项的POJO对象
 * @date 2025/6/15 17:40
 * @Project FileAutoManage(FAM)
 **/

/**
 * 系统配置对象，包含所有系统设置项
 */
public class SystemConfig {
    private Map<String, Object> settings;

    public SystemConfig(Map<String, Object> settings){
        this.settings = settings;
    }

    public Object get(String key){
        return settings.get(key);
    }

    public String getString(String key){
        return (String) settings.get(key);
    }

    public Integer getInt(String key) {
        return (Integer) settings.get(key);
    }

    public Boolean getBoolean(String key) {
        return (Boolean) settings.get(key);
    }

    public void set(String key, Object value) {
        settings.put(key, value);
    }


}
