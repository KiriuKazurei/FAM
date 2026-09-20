#ifndef MAINWINDOW_H
#define MAINWINDOW_H

#include <QWidget>

#include <memory>

#include "bridge.h"
#include "famlistmodel.h"

class QComboBox;
class QLabel;
class QLineEdit;
class QListView;
class QTimer;

class MainWindow final : public QWidget {
    Q_OBJECT

public:
    /* `configPath` empty means the platform config file; `initialQuery` runs a search once shown. */
    explicit MainWindow(const QString &configPath = QString(),
                    const QString &initialQuery = QString(),
                    QWidget *parent = nullptr);

    /* Watches `dir` for file-system changes; returns false with `error` set on failure. */
    bool startWatching(const QString &dir, QString *error);

private slots:
    void runSearch();
    void pollDeltas();
    void watchFolder();
    void tagSelectedFile();

private:
    void setStatus(const QString &text);

    std::unique_ptr<FamBridge> m_bridge;
    FamListModel *m_model = nullptr;
    QLineEdit *m_search = nullptr;
    QComboBox *m_kind = nullptr;
    QListView *m_list = nullptr;
    QLabel *m_status = nullptr;
    QTimer *m_timer = nullptr;
    quint64 m_view = 0;
    quint64 m_watch = 0;
    QString m_watchPath;
};

#endif /* MAINWINDOW_H */
