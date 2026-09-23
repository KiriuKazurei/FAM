#ifndef MAINWINDOW_H
#define MAINWINDOW_H

#include <QElapsedTimer>
#include <QWidget>

#include <memory>

#include "bridge.h"
#include "famlistmodel.h"

class IconCache;
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

    IconCache *icons() const { return m_icons; }

    /* Drives the list through several viewports and lets the paints settle. The headless gate uses
     * it to make the icon worker touch every extension in a folder before the HICON counters are
     * read: a leak is only visible after the code path has actually run. */
    void scrollForSelfCheck();

    /* One file in, one file out, on the folder the self-check is watching. Creating it is what makes
     * the core's reconciled re-read happen while a view is open, which is the only way the shell's
     * reconcile path gets run in a headless gate - and it has to be driven from inside the process,
     * because a harness that creates the file from outside cannot tell whether the run is still
     * alive. Opt-in: nothing writes into a folder the user pointed at unless asked. */
    void createSelfCheckProbe();
    void removeSelfCheckProbe();

private slots:
    void runSearch();
    void pollChanges();
    void openFolderDialog();
    void tagSelectedFile();

private:
    void setStatus(const QString &text);
    /* One poll of a search that is still running: its row count grows, and when it stops growing the
     * rows are re-sorted, which is the last thing the model has to hear about. */
    void pollSearch();

    std::unique_ptr<FamBridge> m_bridge;
    FamListModel *m_model = nullptr;
    IconCache *m_icons = nullptr;
    QLineEdit *m_search = nullptr;
    QComboBox *m_kind = nullptr;
    QListView *m_list = nullptr;
    QLabel *m_status = nullptr;
    QTimer *m_timer = nullptr;
    QStringList m_kinds;
    quint64 m_dirToken = 0;
    quint64 m_searchToken = 0;
    bool m_searchRunning = false;
    QString m_directory;
    /* Set while the opt-in self-check mutation is in the watched folder, so it can be undone. */
    QString m_probe;
    FamWindow m_head;
    /* Everything the shell does after this point is measured against it, so the headless log can say
     * how long a folder took to show rather than leaving that to be guessed from a screenshot. */
    QElapsedTimer m_clock;
};

#endif /* MAINWINDOW_H */
