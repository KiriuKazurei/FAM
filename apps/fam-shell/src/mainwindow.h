#ifndef MAINWINDOW_H
#define MAINWINDOW_H

#include <QElapsedTimer>
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
                        const QString &initialQuery = QString(), QWidget *parent = nullptr);

    ~MainWindow() override;

    /* Shows one directory and keeps it current: the core watches what a view is open on, so opening
     * is the whole of the subscription. Returns false with `error` set on failure. */
    bool openFolder(const QString &dir, QString *error);

private slots:
    void runSearch();
    void pollChanges();
    void openFolderDialog();
    void tagSelectedFile();

private:
    void setStatus(const QString &text);
    /* The v1 JSON view search still answers through, created on the first search. */
    bool ensureSearchView(QString *error);

    std::unique_ptr<FamBridge> m_bridge;
    FamListModel *m_model = nullptr;
    QLineEdit *m_search = nullptr;
    QComboBox *m_kind = nullptr;
    QListView *m_list = nullptr;
    QLabel *m_status = nullptr;
    QTimer *m_timer = nullptr;
    QStringList m_kinds;
    quint64 m_dirToken = 0;
    quint64 m_view = 0;
    QString m_directory;
    FamWindow m_head;
    /* Everything the shell does after this point is measured against it, so the headless log can say
     * how long a folder took to show rather than leaving that to be guessed from a screenshot. */
    QElapsedTimer m_clock;
};

#endif /* MAINWINDOW_H */
