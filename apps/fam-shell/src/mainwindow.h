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

    /* What the run spent on the ABI, for the headless gate to read. */
    FamIpcCount ipcCount() const {
        return m_bridge != nullptr ? m_bridge->ipcCount() : FamIpcCount();
    }

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

    /* The search box, driven from the headless timeline: starting a search while a folder is open is
     * the path that used to close the folder, and clearing the box is the only way back. Neither
     * happens on its own in a run that has no hands on the keyboard. */
    void runSelfCheckSearch(const QString &terms);
    void clearSelfCheckSearch();

private slots:
    void runSearch();
    void pollChanges();
    void openFolderDialog();
    void tagSelectedFile();
    /* Putting the folder back on screen: a search no longer closes the directory it was started in,
     * so clearing the box is what returns to it. */
    void showFolder();

private:
    void setStatus(const QString &text);
    /* The two things the one model can be showing. Each binds itself to its token and takes the
     * header with it, so switching between them is one call from either side. */
    void bindFolderView(QString *error);
    void bindResultsView(QString *error);
    /* The folder's poll. `ownsModel` is false while search results are on screen: the cuts are still
     * drained - a view nobody polls queues them forever - but the model is not told to cut rows it is
     * not showing. */
    void pollFolder(bool ownsModel);
    /* One place words a directory header. The states are opposite claims that used to render the
     * same: a folder with nothing in it, a folder nobody has asked to read, and a folder that
     * refuses to be read (an ACL, a deleted path, a dead UNC share) all looked like a live, empty
     * listing. */
    QString folderStatus(const FamWindow &head, const QString &extra = QString()) const;
    /* What a name search can answer with right now. Re-read rather than remembered: the index
     * backend's state is probed on the core's own thread and can change while the window is open. */
    QString searchBackendNote();
    /* One poll of a search that is still running: its row count grows, and when it stops growing the
     * rows are re-sorted, which is the last thing the model has to hear about. */
    void pollSearch();

    std::unique_ptr<FamBridge> m_bridge;
    FamListModel *m_model = nullptr;
    IconCache *m_icons = nullptr;
    QLineEdit *m_search = nullptr;
    QComboBox *m_kind = nullptr;
    /* Whether a search is scoped to the folder on screen or spans every configured root. Without it,
     * "search here" would be the only thing a search could do once a folder was open - and the
     * previous build could only search everywhere, so either one alone is a regression. */
    QComboBox *m_scope = nullptr;
    QListView *m_list = nullptr;
    QLabel *m_status = nullptr;
    QTimer *m_timer = nullptr;
    QStringList m_kinds;
    QStringList m_backends;
    QStringList m_available;
    quint64 m_dirToken = 0;
    quint64 m_searchToken = 0;
    bool m_searchRunning = false;
    QString m_directory;
    /* Set while the opt-in self-check mutation is in the watched folder, so it can be undone. */
    QString m_probe;
    /* One header per token, because both are polled on the same beat and neither is the other's
     * "before": sharing one variable made the folder's change log compare against a result set's
     * row count, which is how "folder changed behind the results: 0 -> 127" got printed. */
    FamWindow m_folderHead;
    FamWindow m_resultsHead;
    /* Everything the shell does after this point is measured against it, so the headless log can say
     * how long a folder took to show rather than leaving that to be guessed from a screenshot. */
    QElapsedTimer m_clock;
};

#endif /* MAINWINDOW_H */
